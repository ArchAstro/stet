//! The window: winit events in, frames out. Also the off-screen screenshot
//! and benchmark entry points, which reuse the same session and renderer.

use crate::gpu::Gpu;
use crate::images::Images;
use crate::platform::{self, Unsaved};
use crate::session::Session;
use crate::text::Fonts;
use crate::view::{Target, View};
use crate::{Args, build_editor, no_wake};
use md_core::editor::{Message, PaletteKind};
use md_core::input::parse_keys;
use md_core::{Effect, Key, KeyEvent, Mode, Mods};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];
/// Unsaved text is snapshotted this long after the last keystroke.
const RECOVERY_DELAY: Duration = Duration::from_millis(800);

#[derive(Debug)]
enum Wake {
    /// An image finished decoding.
    Redraw,
    /// A line from `--drive`: scripted input for end-to-end checks.
    Drive(String),
    /// A request from `md ctl` or a later `md` launch, and where to reply.
    Request(u64, String, std::sync::mpsc::Sender<String>),
    /// The caller of request `id` hung up before it was answered.
    Gone(u64),
}

struct Running {
    session: Session,
    gpu: Gpu,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    instance: wgpu::Instance,
    mods: ModifiersState,
    mouse: (f32, f32),
    dragging: bool,
    last_click: Option<(Instant, (f32, f32), u8)>,
    title: String,
    ime_allowed: bool,
    recovery_due: Option<Instant>,
    drawn: bool,
    /// This process answers later `md` launches.
    listening: bool,
    /// Assistants blocked in `md ctl wait`, oldest first.
    waiters: std::collections::VecDeque<Waiter>,
    /// Messages sent while the assistant was busy with an earlier one.
    outbox: std::collections::VecDeque<String>,
    /// The assistant took a message and has not come back for another yet.
    working: Option<(String, Instant)>,
    // Dropped last, after the surface that points into it.
    window: Arc<Window>,
}

/// What can be made ready while the OS is still creating the window.
pub struct Prepared {
    editor: md_core::Editor,
    fonts: std::thread::JoinHandle<Fonts>,
}

/// Reads the config and document, and starts loading fonts on another
/// thread: scanning them, and shaping the first screen of text once so the
/// font data and shaping plans are warm when the window appears.
pub fn prepare(args: &Args) -> Prepared {
    md_core::highlight::warm();
    let (editor, font_dirs) = build_editor(args);
    crate::timing("config and file read");
    let (config, colors) = (editor.config.clone(), editor.theme.colors);
    let sample: Vec<String> = (0..editor.buf.line_count().min(80))
        .map(|line| editor.buf.line_text(line))
        .collect();
    let fonts = std::thread::spawn(move || {
        let mut fonts = Fonts::new(&config, &font_dirs);
        crate::timing("fonts loaded");
        // Most displays that matter here are 2x; a miss only costs the warm-up.
        let font = (config.font_size * 2.0).round();
        let shape =
            |fonts: &mut Fonts, text: &str, block: md_core::markdown::Block, face: Option<crate::text::Face>| {
                crate::text::layout_line(
                    fonts,
                    &crate::text::LineSpec {
                        text,
                        spans: &[],
                        block,
                        font,
                        line: (font * config.line_height).round(),
                        width: font * 40.0,
                        colors: &colors,
                        dim: false,
                        face,
                        bold: false,
                    },
                );
            };
        for line in &sample {
            shape(&mut fonts, line, md_core::markdown::Block::Text, None);
        }
        let pangram = "The quick brown fox jumps over the lazy dog 0123456789 #*_`[](){}<>|~-+=.,:;!?/";
        shape(&mut fonts, pangram, md_core::markdown::Block::Heading(1), None);
        shape(&mut fonts, pangram, md_core::markdown::Block::Code, None);
        shape(
            &mut fonts,
            pangram,
            md_core::markdown::Block::Text,
            Some(crate::text::Face::Ui),
        );
        crate::timing("fonts warm");
        fonts
    });
    Prepared { editor, fonts }
}

struct Waiter {
    id: u64,
    name: String,
    reply: std::sync::mpsc::Sender<String>,
}

/// An assistant that took a message and never returned is forgotten.
const WORKING_LIMIT: Duration = Duration::from_secs(30 * 60);

struct App {
    listening: bool,
    prepared: Option<Prepared>,
    proxy: EventLoopProxy<Wake>,
    running: Option<Running>,
    error: Option<String>,
}

pub fn run(args: Args) -> Result<(), String> {
    let prepared = prepare(&args);
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .map_err(|err| err.to_string())?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    if args.drive {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                if proxy.send_event(Wake::Drive(line)).is_err() {
                    break;
                }
            }
        });
    }
    let listening = !args.new_window && {
        let proxy = proxy.clone();
        let next_id = std::sync::atomic::AtomicU64::new(1);
        platform::listen(move |request, alive| {
            let id = next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (reply, answer) = std::sync::mpsc::channel();
            if proxy.send_event(Wake::Request(id, request, reply)).is_err() {
                return r#"{"ok":false,"error":"md is closing"}"#.to_string();
            }
            // Most requests are answered at once; an assistant waiting for a
            // message is answered when the writer sends one.
            loop {
                match answer.recv_timeout(Duration::from_millis(400)) {
                    Ok(reply) => return reply,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) if alive() => {}
                    Err(_) => {
                        let _ = proxy.send_event(Wake::Gone(id));
                        return String::new();
                    }
                }
            }
        })
    };
    let mut app = App {
        listening,
        prepared: Some(prepared),
        proxy,
        running: None,
        error: None,
    };
    event_loop.run_app(&mut app).map_err(|err| err.to_string())?;
    app.error.map_or(Ok(()), Err)
}

fn map_key(event: &winit::event::KeyEvent, mods: ModifiersState) -> Option<KeyEvent> {
    let key = match &event.logical_key {
        WinitKey::Named(named) => match named {
            NamedKey::Enter => Key::Enter,
            NamedKey::Escape => Key::Esc,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::Tab => Key::Tab,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::Space => Key::Char(' '),
            _ => return None,
        },
        WinitKey::Character(text) => Key::Char(text.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent {
        key,
        mods: Mods {
            ctrl: mods.control_key(),
            alt: mods.alt_key(),
            shift: mods.shift_key(),
            sup: mods.super_key(),
        },
    })
}

fn error(text: String) -> Option<Message> {
    Some(Message { text, error: true })
}

impl Running {
    fn new(event_loop: &ActiveEventLoop, prepared: Prepared, proxy: EventLoopProxy<Wake>) -> Result<Running, String> {
        crate::timing("event loop ready");
        let Prepared { mut editor, fonts } = prepared;
        let state = platform::State::load();
        let size = state.window.unwrap_or((1040.0, 760.0));
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut attributes = Window::default_attributes()
            .with_title("md")
            .with_inner_size(LogicalSize::new(
                size.0.clamp(360.0, 8000.0),
                size.1.clamp(240.0, 8000.0),
            ))
            .with_min_inner_size(LogicalSize::new(360.0, 240.0));
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes = attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true);
        }
        let window = Arc::new(event_loop.create_window(attributes).map_err(|err| err.to_string())?);
        crate::timing("window created");

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone()).map_err(|err| err.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|err| format!("no graphics adapter: {err}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|err| err.to_string())?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or(capabilities.formats.first().copied())
            .ok_or("the surface supports no texture formats")?;
        // Frames are drawn on demand, so nothing is gained by queueing them
        // behind the display's refresh: show each as soon as it is ready.
        // Mailbox never tears; macOS composites every window, so immediate
        // presentation cannot tear there either.
        let present_mode = if capabilities.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else if cfg!(target_os = "macos") && capabilities.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else {
            wgpu::PresentMode::AutoVsync
        };
        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &surface_config);
        crate::timing("gpu ready");

        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = proxy.send_event(Wake::Redraw);
        });
        let fonts = fonts.join().map_err(|_| "loading fonts failed")?;
        editor.font_families = fonts.families();
        editor.recover_untitled();
        if editor.config.sidebar {
            editor.toggle_sidebar();
            editor.sidebar.focused = false;
        }
        let mut view = View::new(fonts, Images::new(wake, platform::cache_dir()));
        view.scale = window.scale_factor() as f32;
        view.zoom = state.zoom.unwrap_or(1.0).clamp(0.5, 4.0);
        view.titlebar = if cfg!(target_os = "macos") {
            28.0 * view.scale
        } else {
            0.0
        };
        (view.width, view.height) = (size.width as f32, size.height as f32);
        let mut running = Running {
            session: Session { editor, view },
            gpu: Gpu::new(device, queue, format),
            surface,
            surface_config,
            instance,
            mods: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            dragging: false,
            last_click: None,
            title: String::new(),
            ime_allowed: false,
            recovery_due: None,
            drawn: false,
            listening: false,
            waiters: Default::default(),
            outbox: Default::default(),
            working: None,
            window,
        };
        crate::timing("renderer built");
        running.session.settle();
        running.after_input();
        crate::timing("ready for first frame");
        Ok(running)
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width.max(1);
        self.surface_config.height = height.max(1);
        self.surface.configure(&self.gpu.device, &self.surface_config);
        let view = &mut self.session.view;
        (view.width, view.height) = (width as f32, height as f32);
        view.scale = self.window.scale_factor() as f32;
        view.titlebar = if cfg!(target_os = "macos") && self.window.fullscreen().is_none() {
            28.0 * view.scale
        } else {
            0.0
        };
        view.follow_cursor(&mut self.session.editor);
        self.window.request_redraw();
    }

    /// Keeps the window chrome in step with the editor after any input.
    fn after_input(&mut self) {
        let editor = &self.session.editor;
        let title = format!(
            "{}{} — md",
            editor.file_name(),
            if editor.buf.is_dirty() { " •" } else { "" }
        );
        if title != self.title {
            self.window.set_title(&title);
            self.title = title;
        }
        // Composition belongs to typing, not to normal-mode commands.
        let ime = editor.mode == Mode::Insert || editor.cmdline.is_some() || editor.palette.is_some();
        if ime != self.ime_allowed {
            self.window.set_ime_allowed(ime);
            self.ime_allowed = ime;
        }
        self.recovery_due = Some(Instant::now() + RECOVERY_DELAY);
        self.window.request_redraw();
    }

    fn redraw(&mut self) {
        for image in self.session.view.images.poll() {
            self.gpu.upload_image(image.id, image.width, image.height, &image.rgba);
        }
        if !self.drawn {
            crate::timing("first redraw requested");
        }
        let frame = self.session.view.frame(&mut self.session.editor);
        if !self.drawn {
            crate::timing("first frame laid out");
        }
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.gpu.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
                    self.surface = surface;
                    self.surface.configure(&self.gpu.device, &self.surface_config);
                    self.window.request_redraw();
                }
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => return,
        };
        if !self.drawn {
            crate::timing("first surface texture acquired");
        }
        let target = texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let size = (self.surface_config.width, self.surface_config.height);
        let view = &mut self.session.view;
        self.gpu.render(&target, size, &frame, &view.layouts, &mut view.fonts);
        self.gpu.queue.present(texture);
        if !std::mem::replace(&mut self.drawn, true) {
            crate::timing("first frame presented");
        }
    }

    fn open(&mut self, path: &Path) {
        let editor = &mut self.session.editor;
        if let Err(err) = editor.open_path(path) {
            editor.message = error(err);
        }
    }

    fn save_as(&mut self) -> bool {
        let Some(path) = platform::pick_save(&self.session.editor.file_name()) else {
            return false;
        };
        let editor = &mut self.session.editor;
        match editor.save(Some(&path), true) {
            Ok(()) => true,
            Err(err) => {
                editor.message = error(err);
                false
            }
        }
    }

    /// True when the active document may be discarded or was saved.
    fn confirm_discard(&mut self) -> bool {
        if !self.session.editor.buf.is_dirty() {
            return true;
        }
        match platform::confirm_unsaved(&self.session.editor.file_name()) {
            Unsaved::Discard => true,
            Unsaved::Cancel => false,
            Unsaved::Save if self.session.editor.path.is_none() => self.save_as(),
            Unsaved::Save => self.session.editor.save(None, false).is_ok(),
        }
    }

    /// Asks about every unsaved document; false if the user backs out.
    fn confirm_all(&mut self) -> bool {
        for index in self.session.editor.dirty_tabs() {
            self.session.editor.switch_tab(index);
            self.session.settle();
            self.redraw();
            if !self.confirm_discard() {
                return false;
            }
        }
        true
    }

    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        platform::stop_listening(self.listening);
        self.save_state();
        self.session.editor.shutdown();
        event_loop.exit();
    }

    fn save_state(&self) {
        let editor = &self.session.editor;
        let size = self.window.inner_size().to_logical::<f32>(self.window.scale_factor());
        platform::State {
            theme: Some(editor.theme.name.clone()),
            zoom: Some(self.session.view.zoom),
            prose_font: editor.config.prose_font.first().cloned(),
            mono_font: editor.config.mono_font.first().cloned(),
            window: self.window.fullscreen().is_none().then_some((size.width, size.height)),
        }
        .save();
    }

    fn effects(&mut self, effects: Vec<Effect>, event_loop: &ActiveEventLoop) {
        let mut queue: std::collections::VecDeque<Effect> = effects.into();
        while let Some(effect) = queue.pop_front() {
            match effect {
                Effect::Quit => return self.quit(event_loop),
                Effect::Close if self.confirm_discard() => {
                    self.session.editor.close_tab(true);
                    queue.extend(self.session.settle());
                }
                Effect::CloseAll if self.confirm_all() => return self.quit(event_loop),
                Effect::OpenDialog => {
                    if let Some(path) = platform::pick_file() {
                        self.open(&path);
                        queue.extend(self.session.settle());
                    }
                }
                Effect::SaveAsDialog => {
                    self.save_as();
                }
                Effect::OpenUrl(url) => {
                    if let Err(err) = open::that_detached(&url) {
                        self.session.editor.message = error(format!("cannot open {url}: {err}"));
                    }
                }
                Effect::ToggleFullscreen => {
                    let next = self
                        .window
                        .fullscreen()
                        .is_none()
                        .then_some(Fullscreen::Borderless(None));
                    self.window.set_fullscreen(next);
                }
                Effect::AgentMessage { text, selection } => self.agent_message(text, selection),
                Effect::ThemeChanged | Effect::FontChanged | Effect::Zoom(_) => self.save_state(),
                _ => {}
            }
        }
        self.after_input();
    }

    /// Tells the editor who is connected, for the status line and the prompt.
    fn agent_changed(&mut self) {
        if self
            .working
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() > WORKING_LIMIT)
        {
            self.working = None;
            self.outbox.clear();
        }
        let agent = match (self.waiters.front(), &self.working) {
            (Some(waiter), _) => Some(md_core::editor::Agent {
                name: waiter.name.clone(),
                listening: true,
            }),
            (None, Some((name, _))) => Some(md_core::editor::Agent {
                name: name.clone(),
                listening: false,
            }),
            (None, None) => None,
        };
        if self.session.editor.agent != agent {
            self.session.editor.agent = agent;
            self.window.request_redraw();
        }
    }

    /// One request from the socket.
    fn request(
        &mut self,
        id: u64,
        request: &str,
        reply: std::sync::mpsc::Sender<String>,
        event_loop: &ActiveEventLoop,
    ) {
        let parsed: serde_json::Value = serde_json::from_str(request).unwrap_or_default();
        if parsed["cmd"] == "wait" {
            let name = parsed["name"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Claude")
                .to_string();
            self.working = None;
            match self.outbox.pop_front() {
                Some(message) => {
                    let _ = reply.send(message);
                    self.working = Some((name, Instant::now()));
                }
                None => self.waiters.push_back(Waiter { id, name, reply }),
            }
            return self.agent_changed();
        }
        let (answer, raise) = crate::ctl::handle(&mut self.session.editor, request);
        let _ = reply.send(answer.to_string());
        let effects = self.session.settle();
        self.effects(effects, event_loop);
        if raise {
            self.window.set_minimized(false);
            self.window.focus_window();
        }
    }

    /// Hands the writer's message to the assistant, or queues it if the
    /// assistant is still busy with the last one.
    fn agent_message(&mut self, text: String, selection: Option<(std::ops::Range<usize>, String)>) {
        let message = crate::ctl::message(&mut self.session.editor, &text, selection).to_string();
        let mut message = Some(message);
        while let (Some(waiter), Some(text)) = (self.waiters.pop_front(), message.take()) {
            match waiter.reply.send(text) {
                Ok(()) => self.working = Some((waiter.name, Instant::now())),
                Err(unsent) => message = Some(unsent.0),
            }
        }
        let editor = &mut self.session.editor;
        match (message, &self.working) {
            (None, Some((name, _))) => {
                editor.message = Some(Message {
                    text: format!("sent to {name}"),
                    error: false,
                })
            }
            (Some(message), Some((name, _))) => {
                editor.message = Some(Message {
                    text: format!("{name} is busy; queued"),
                    error: false,
                });
                self.outbox.push_back(message);
            }
            _ => editor.message = error("no assistant is connected (one connects with `md ctl wait`)".to_string()),
        }
        self.agent_changed();
    }

    fn key(&mut self, key: KeyEvent, event_loop: &ActiveEventLoop) {
        let effects = self.session.key(key);
        self.effects(effects, event_loop);
    }

    fn mouse_press(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 6.0 && (a.1 - b.1).abs() < 6.0;
        let clicks = match self.last_click {
            Some((at, place, clicks))
                if now - at < Duration::from_millis(450) && near(place, self.mouse) && clicks < 3 =>
            {
                clicks + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, self.mouse, clicks));
        let session = &mut self.session;
        let command = if cfg!(target_os = "macos") {
            self.mods.super_key()
        } else {
            self.mods.control_key()
        };
        let editor = &mut session.editor;
        let target = session.view.target_at(self.mouse.0, self.mouse.1);
        if editor.context_menu.is_some() && !matches!(target, Some(Target::ContextRow(_) | Target::ContextMenu)) {
            // A click elsewhere only dismisses the menu.
            editor.close_context_menu();
            return self.effects(Vec::new(), event_loop);
        }
        match target {
            Some(Target::ContextRow(index)) => editor.menu_click(index),
            Some(Target::ContextMenu) => {}
            Some(Target::PaletteRow(index)) => editor.palette_click(index),
            Some(Target::Palette) => {}
            Some(Target::Scrim) => editor.close_palette(),
            Some(Target::Tab(index)) => editor.switch_tab(index),
            Some(Target::SidebarRow(index)) => editor.sidebar_click(index, command),
            Some(Target::Sidebar) => editor.sidebar.focused = true,
            Some(Target::MenuHint) => editor.open_palette(PaletteKind::Help),
            None => {
                let pos = session.view.hit(editor, self.mouse.0, self.mouse.1);
                if command && clicks == 1 && editor.follow_link_at(pos) {
                    // Followed; nothing to select.
                } else {
                    self.dragging = clicks == 1;
                    editor.mouse_down(pos, clicks, self.mods.shift_key());
                }
            }
        }
        session.view.goal_x = None;
        let effects = session.settle();
        self.effects(effects, event_loop);
    }

    /// A right-click: the menu for whatever is under the pointer.
    fn context_click(&mut self, event_loop: &ActiveEventLoop) {
        let session = &mut self.session;
        let (x, y) = self.mouse;
        let editor = &mut session.editor;
        editor.close_context_menu();
        match session.view.target_at(x, y) {
            Some(Target::SidebarRow(index)) => editor.sidebar_menu(index, x, y),
            Some(Target::Tab(index)) => editor.tab_menu(index, x, y),
            None if editor.palette.is_none() => {
                let pos = session.view.hit(editor, x, y);
                editor.context_menu_at(pos, x, y);
            }
            _ => {}
        }
        let effects = session.settle();
        self.effects(effects, event_loop);
    }

    fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        if self.dragging {
            let session = &mut self.session;
            let pos = session.view.hit(&mut session.editor, x, y);
            session.editor.mouse_drag(pos);
            self.after_input();
        }
    }

    fn mouse_release(&mut self) {
        self.dragging = false;
        self.session.editor.mouse_up();
    }

    fn wheel(&mut self, pixels: f32) {
        let session = &mut self.session;
        let over = session.view.target_at(self.mouse.0, self.mouse.1);
        if session.editor.palette.is_some() {
            return;
        }
        if matches!(over, Some(Target::Sidebar | Target::SidebarRow(_))) {
            let rows = (pixels / (28.0 * session.view.scale)).round() as isize;
            session.view.scroll_sidebar(
                &mut session.editor,
                if rows == 0 { pixels.signum() as isize } else { rows },
            );
        } else {
            session.view.scroll_by(&mut session.editor, pixels);
        }
        self.window.request_redraw();
    }

    fn drop_file(&mut self, path: &Path, event_loop: &ActiveEventLoop) {
        let is_image = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
        let effects = if is_image {
            // Prefer a path relative to the document, so the pair stays portable.
            let base = self.session.editor.path.as_deref().and_then(Path::parent);
            let shown = base.and_then(|base| path.strip_prefix(base).ok()).unwrap_or(path);
            let target = shown.to_string_lossy().replace(' ', "%20");
            self.session.text(&format!("![]({target})"))
        } else {
            self.open(path);
            self.session.settle()
        };
        self.effects(effects, event_loop);
    }

    /// One scripted input line. Positions are in logical points.
    fn drive(&mut self, line: &str, event_loop: &ActiveEventLoop) {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let numbers: Vec<f32> = rest.split_whitespace().filter_map(|word| word.parse().ok()).collect();
        let scale = self.window.scale_factor() as f32;
        let point = |at: usize| {
            (
                numbers.get(at).copied().unwrap_or(0.0) * scale,
                numbers.get(at + 1).copied().unwrap_or(0.0) * scale,
            )
        };
        match command {
            "keys" => {
                for key in parse_keys(rest) {
                    self.key(key, event_loop);
                }
            }
            "text" => {
                let effects = self.session.text(rest);
                self.effects(effects, event_loop);
            }
            "click" => {
                let (x, y) = point(0);
                self.mouse_move(x, y);
                self.mods = match rest.split_whitespace().last() {
                    Some("cmd") => ModifiersState::SUPER,
                    Some("ctrl") => ModifiersState::CONTROL,
                    Some("shift") => ModifiersState::SHIFT,
                    _ => ModifiersState::empty(),
                };
                for _ in 0..numbers.get(2).map_or(1, |count| *count as usize).clamp(1, 3) {
                    self.mouse_press(event_loop);
                    self.mouse_release();
                }
                self.mods = ModifiersState::empty();
            }
            "rclick" => {
                let (x, y) = point(0);
                self.mouse_move(x, y);
                self.context_click(event_loop);
            }
            "drag" => {
                let ((x0, y0), (x1, y1)) = (point(0), point(2));
                self.mouse_move(x0, y0);
                self.last_click = None;
                self.mouse_press(event_loop);
                for step in 1..=8 {
                    let t = step as f32 / 8.0;
                    self.mouse_move(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                }
                self.mouse_release();
            }
            "scroll" => {
                let (x, y) = point(0);
                self.mouse_move(x, y);
                self.wheel(numbers.get(2).copied().unwrap_or(0.0) * scale);
            }
            "resize" => {
                let _ = self.window.request_inner_size(LogicalSize::new(
                    numbers.first().copied().unwrap_or(1040.0),
                    numbers.get(1).copied().unwrap_or(760.0),
                ));
            }
            "drop" => self.drop_file(Path::new(rest.trim()), event_loop),
            "shot" => {
                // What the window shows now, straight from its own renderer.
                self.redraw();
                if let Err(err) = capture(&mut self.gpu, &mut self.session, Path::new(rest.trim())) {
                    eprintln!("md: shot: {err}");
                }
            }
            "quit" => self.quit(event_loop),
            _ => {}
        }
        self.window.request_redraw();
    }

    fn event(&mut self, event: WindowEvent, event_loop: &ActiveEventLoop) {
        match event {
            WindowEvent::CloseRequested => self.effects(vec![Effect::CloseAll], event_loop),
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::ScaleFactorChanged { .. } => {
                self.session.view.invalidate();
                let size = self.window.inner_size();
                self.resize(size.width, size.height);
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key {
                    WinitKey::Named(NamedKey::F11) => return self.effects(vec![Effect::ToggleFullscreen], event_loop),
                    WinitKey::Named(NamedKey::ContextMenu) | WinitKey::Named(NamedKey::F10)
                        if event.logical_key == WinitKey::Named(NamedKey::ContextMenu) || self.mods.shift_key() =>
                    {
                        self.session.editor.open_context_menu(md_core::editor::MenuAt::Cursor);
                        return self.effects(Vec::new(), event_loop);
                    }
                    WinitKey::Named(NamedKey::F1) => {
                        self.session.editor.open_palette(PaletteKind::Help);
                        return self.effects(Vec::new(), event_loop);
                    }
                    _ => {}
                }
                if let Some(key) = map_key(&event, self.mods) {
                    self.key(key, event_loop);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                let effects = self.session.text(&text);
                self.effects(effects, event_loop);
            }
            WindowEvent::CursorMoved { position, .. } => self.mouse_move(position.x as f32, position.y as f32),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.mouse_press(event_loop),
                ElementState::Released => self.mouse_release(),
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self.context_click(event_loop),
            WindowEvent::MouseWheel { delta, .. } => {
                let session = &self.session;
                let line = session.editor.config.font_size * session.editor.config.line_height * session.view.scale;
                self.wheel(match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * line * 3.0,
                    MouseScrollDelta::PixelDelta(PhysicalPosition { y, .. }) => -y as f32,
                });
            }
            WindowEvent::DroppedFile(path) => self.drop_file(&path, event_loop),
            WindowEvent::Focused(true) => {
                self.session.editor.check_disk();
                self.session.editor.refresh_sidebar();
                self.session.view.images.retry_failed();
                self.session.settle();
                self.after_input();
            }
            WindowEvent::Focused(false) => self.session.editor.write_recovery(),
            _ => {}
        }
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        let Some(prepared) = self.prepared.take() else { return };
        match Running::new(event_loop, prepared, self.proxy.clone()) {
            Ok(mut running) => {
                running.listening = self.listening;
                self.running = Some(running);
            }
            Err(err) => {
                self.error = Some(err);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let Some(running) = &mut self.running {
            running.event(event, event_loop);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        let Some(running) = &mut self.running else { return };
        match wake {
            Wake::Redraw => running.window.request_redraw(),
            Wake::Drive(line) => running.drive(&line, event_loop),
            Wake::Request(id, request, reply) => running.request(id, &request, reply, event_loop),
            Wake::Gone(id) => {
                running.waiters.retain(|waiter| waiter.id != id);
                running.agent_changed();
            }
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(running)) = (cause, &mut self.running) {
            running.recovery_due = None;
            running.session.editor.write_recovery();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let due = self.running.as_ref().and_then(|running| running.recovery_due);
        event_loop.set_control_flow(due.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

fn headless_session(args: &Args) -> Session {
    let (editor, font_dirs) = build_editor(args);
    let mut images = Images::new(no_wake(), None);
    images.blocking = true;
    let mut view = View::new(Fonts::new(&editor.config, &font_dirs), images);
    view.scale = args.scale;
    view.width = (args.size.0 as f32 * args.scale).round();
    view.height = (args.size.1 as f32 * args.scale).round();
    Session { editor, view }
}

/// Renders one frame to a PNG without opening a window.
pub fn screenshot(args: &Args) -> Result<(), String> {
    let out = args.screenshot.as_ref().ok_or("no output path")?;
    let mut session = headless_session(args);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map_err(|err| format!("no graphics adapter: {err}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|err| err.to_string())?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut gpu = Gpu::new(device, queue, format);

    session.settle();
    for key in parse_keys(&args.keys) {
        session.key(key);
    }
    // The first frame requests images; the second has them.
    session.view.frame(&mut session.editor);
    for image in session.view.images.poll() {
        gpu.upload_image(image.id, image.width, image.height, &image.rgba);
    }
    session.view.follow_cursor(&mut session.editor);

    capture(&mut gpu, &mut session, out)
}

/// Renders the session's current frame into a PNG.
fn capture(gpu: &mut Gpu, session: &mut Session, out: &Path) -> Result<(), String> {
    let frame = session.view.frame(&mut session.editor);
    let (width, height) = (session.view.width as u32, session.view.height as u32);
    let format = gpu.format;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
    gpu.render(
        &target,
        (width, height),
        &frame,
        &session.view.layouts,
        &mut session.view.fonts,
    );

    let stride = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (stride * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|err| err.to_string())?;
    let mapped = buffer.slice(..).get_mapped_range().map_err(|err| err.to_string())?;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in mapped.chunks(stride as usize) {
        pixels.extend_from_slice(&row[..(width * 4) as usize]);
    }
    if matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        pixels.chunks_exact_mut(4).for_each(|pixel| pixel.swap(0, 2));
    }
    image::save_buffer(out, &pixels, width, height, image::ColorType::Rgba8).map_err(|err| err.to_string())
}

/// Prints how long the hot paths take on the given file.
pub fn bench(args: &Args) {
    let started = Instant::now();
    let mut session = headless_session(args);
    println!("startup (config, fonts, file)   {:>9.2?}", started.elapsed());
    let [prose, mono, ui] = session.view.fonts.names();
    println!("fonts (prose / code / interface) {prose} / {mono} / {ui}");
    let editor = &mut session.editor;
    let text = editor.buf.text();
    println!(
        "document                        {} bytes, {} lines",
        text.len(),
        editor.buf.line_count()
    );

    let time = |label: &str, rounds: u32, work: &mut dyn FnMut()| {
        let started = Instant::now();
        for _ in 0..rounds {
            work();
        }
        println!("{label:<32}{:>9.2?}", started.elapsed() / rounds);
    };
    time("markdown analysis", 20, &mut || {
        std::hint::black_box(md_core::markdown::parse(&text));
    });
    time("  of which suggestions", 20, &mut || {
        std::hint::black_box(md_core::critic::parse(&text));
    });
    time("buffer to string", 20, &mut || {
        std::hint::black_box(session.editor.buf.text());
    });
    session.settle();
    // The middle of the document, on the next line of ordinary prose.
    let middle = parse_keys(&format!(
        ":{}<CR>/^[a-z]+ [a-z]+ [a-z]<CR>:noh<CR>",
        session.editor.buf.line_count() / 2
    ));
    for key in middle {
        session.key(key);
    }
    time("first frame (cold layout)", 1, &mut || {
        std::hint::black_box(session.view.frame(&mut session.editor));
    });
    time("frame (warm)", 200, &mut || {
        std::hint::black_box(session.view.frame(&mut session.editor));
    });
    session.key(KeyEvent::new(Key::Char('i')));
    time("keystroke → frame built", 200, &mut || {
        session.key(KeyEvent::new(Key::Char('x')));
        std::hint::black_box(session.view.frame(&mut session.editor));
    });
    time("  of which analysis", 200, &mut || {
        session
            .editor
            .buf
            .replace(session.editor.cursor..session.editor.cursor, "x");
        session.editor.refresh();
    });
    time("  of which key handling", 200, &mut || {
        session.editor.handle_key(KeyEvent::new(Key::Char('x')));
    });
    session.editor.refresh();
    time("page down → frame built", 50, &mut || {
        session.key(KeyEvent::new(Key::PageDown));
        std::hint::black_box(session.view.frame(&mut session.editor));
    });

    let recovery = std::env::temp_dir().join(format!("md-bench-recovery-{}", std::process::id()));
    session.editor.recovery_dir = Some(recovery.clone());
    time("recovery snapshot", 10, &mut || {
        session.editor.buf.replace(0..0, "x");
        session.editor.write_recovery();
    });
    session.editor.recovery_dir = None;
    let _ = std::fs::remove_dir_all(recovery);

    // Drawing: what the renderer spends on the CPU per frame.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())) else {
        return;
    };
    let mut gpu = Gpu::new(device, queue, wgpu::TextureFormat::Bgra8UnormSrgb);
    let (width, height) = (session.view.width as u32, session.view.height as u32);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: gpu.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
    for key in parse_keys("<Esc>gg") {
        session.key(key);
    }
    let frame = session.view.frame(&mut session.editor);
    time("draw (first, rasterizes glyphs)", 1, &mut || {
        gpu.render(
            &target,
            (width, height),
            &frame,
            &session.view.layouts,
            &mut session.view.fonts,
        );
    });
    time("draw (warm)", 200, &mut || {
        gpu.render(
            &target,
            (width, height),
            &frame,
            &session.view.layouts,
            &mut session.view.fonts,
        );
    });
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
}
