//! The renderer: rounded rectangles, images and glyphon text in one pass.
//! It draws a `Frame` into any texture view, so the window and the headless
//! screenshot path share every pixel.

use crate::text::{Fonts, LineLayout};
use crate::view::{Frame, Quad};
use bytemuck::{Pod, Zeroable};
use glyphon::{Cache, Resolution, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport};
use std::collections::HashMap;

const SHADER: &str = r#"
struct Globals { size: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VIn {
    @location(0) pos: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) half: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) uv: vec2<f32>,
    @location(5) radius: f32,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) half: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) radius: f32,
};

@vertex fn vs(in: VIn) -> VOut {
    var out: VOut;
    out.clip = vec4<f32>(in.pos.x / globals.size.x * 2.0 - 1.0, 1.0 - in.pos.y / globals.size.y * 2.0, 0.0, 1.0);
    out.local = in.local;
    out.half = in.half;
    out.color = in.color;
    out.uv = in.uv;
    out.radius = in.radius;
    return out;
}

@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    // Signed distance to a rounded box, for antialiased corners.
    let q = abs(in.local) - in.half + vec2<f32>(in.radius);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - in.radius;
    let coverage = clamp(0.5 - d, 0.0, 1.0);
    let c = in.color * textureSample(tex, samp, in.uv);
    return vec4<f32>(c.rgb * c.a * coverage, c.a * coverage);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    local: [f32; 2],
    half: [f32; 2],
    color: [f32; 4],
    uv: [f32; 2],
    radius: f32,
}

fn push_quad(out: &mut Vec<Vertex>, quad: &Quad) {
    let [x, y, w, h] = quad.rect;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let half = [w / 2.0, h / 2.0];
    let radius = quad.radius.min(half[0]).min(half[1]);
    let corner = |u: f32, v: f32| Vertex {
        pos: [x + w * u, y + h * v],
        local: [(u - 0.5) * w, (v - 0.5) * h],
        half,
        color: quad.color,
        uv: [u, v],
        radius,
    };
    let (a, b, c, d) = (corner(0.0, 0.0), corner(1.0, 0.0), corner(1.0, 1.0), corner(0.0, 1.0));
    out.extend([a, b, c, a, c, d]);
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white: wgpu::BindGroup,
    textures: HashMap<u64, wgpu::BindGroup>,
    vertices: wgpu::Buffer,
    vertex_capacity: u64,
    viewport: Viewport,
    atlas: TextAtlas,
    /// One per frame layer.
    text: [TextRenderer; 2],
    pub format: wgpu::TextureFormat,
}

impl Gpu {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Gpu {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quads"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x4, 4 => Float32x2, 5 => Float32
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quads"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let vertex_capacity = 64 * 1024;
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: vertex_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text = [(); 2].map(|()| TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None));
        let white = texture_bind(&device, &queue, &texture_layout, &sampler, 1, 1, &[255; 4]);
        Gpu {
            device,
            queue,
            pipeline,
            globals,
            globals_bind,
            texture_layout,
            sampler,
            white,
            textures: HashMap::new(),
            vertices,
            vertex_capacity,
            viewport,
            atlas,
            text,
            format,
        }
    }

    pub fn upload_image(&mut self, id: u64, width: u32, height: u32, rgba: &[u8]) {
        let bind = texture_bind(
            &self.device,
            &self.queue,
            &self.texture_layout,
            &self.sampler,
            width,
            height,
            rgba,
        );
        self.textures.insert(id, bind);
    }

    pub fn render(
        &mut self,
        target: &wgpu::TextureView,
        size: (u32, u32),
        frame: &Frame,
        layouts: &HashMap<u64, LineLayout>,
        fonts: &mut Fonts,
    ) {
        let (width, height) = size;
        self.queue.write_buffer(
            &self.globals,
            0,
            bytemuck::cast_slice(&[width as f32, height as f32, 0.0, 0.0]),
        );

        // One vertex buffer; per layer: back quads, one quad per image, front quads.
        let layers = [&frame.base, &frame.over];
        let mut vertices = Vec::new();
        let mut draws = Vec::new();
        for layer in layers {
            let start = vertices.len() as u32;
            layer.back.iter().for_each(|quad| push_quad(&mut vertices, quad));
            let back = start..vertices.len() as u32;
            let mut images = Vec::new();
            for (id, rect) in &layer.images {
                if self.textures.contains_key(id) {
                    let start = vertices.len() as u32;
                    push_quad(
                        &mut vertices,
                        &Quad {
                            rect: *rect,
                            color: [1.0; 4],
                            radius: 4.0,
                        },
                    );
                    images.push((*id, start..vertices.len() as u32));
                }
            }
            let start = vertices.len() as u32;
            layer.front.iter().for_each(|quad| push_quad(&mut vertices, quad));
            draws.push((back, images, start..vertices.len() as u32));
        }
        let bytes: &[u8] = bytemuck::cast_slice(&vertices);
        if bytes.len() as u64 > self.vertex_capacity {
            self.vertex_capacity = (bytes.len() as u64).next_power_of_two();
            self.vertices = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: self.vertex_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.queue.write_buffer(&self.vertices, 0, bytes);

        self.viewport.update(&self.queue, Resolution { width, height });
        let mut prepared = [false; 2];
        for (index, layer) in layers.iter().enumerate() {
            let areas = layer.texts.iter().filter_map(|item| {
                let layout = layouts.get(&item.key)?;
                Some(TextArea {
                    buffer: &layout.buffer,
                    left: item.left,
                    top: item.top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: item.clip[0] as i32,
                        top: item.clip[1] as i32,
                        right: item.clip[2] as i32,
                        bottom: item.clip[3] as i32,
                    },
                    default_color: item.color,
                    custom_glyphs: &[],
                })
            });
            prepared[index] = self.text[index]
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut fonts.system,
                    &mut self.atlas,
                    &self.viewport,
                    areas,
                    &mut fonts.swash,
                )
                .is_ok();
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(frame.clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (index, (back, images, front)) in draws.into_iter().enumerate() {
                let quads = |pass: &mut wgpu::RenderPass| {
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &self.globals_bind, &[]);
                    pass.set_vertex_buffer(0, self.vertices.slice(..));
                    pass.set_bind_group(1, &self.white, &[]);
                };
                quads(&mut pass);
                pass.draw(back, 0..1);
                if !images.is_empty() {
                    let [x, y, w, h] = layers[index].image_clip;
                    let (x, y) = ((x.max(0.0) as u32).min(width), (y.max(0.0) as u32).min(height));
                    pass.set_scissor_rect(
                        x,
                        y,
                        (w.max(0.0) as u32).min(width - x),
                        (h.max(0.0) as u32).min(height - y),
                    );
                    for (id, range) in images {
                        pass.set_bind_group(1, &self.textures[&id], &[]);
                        pass.draw(range, 0..1);
                    }
                    pass.set_scissor_rect(0, 0, width, height);
                }
                if prepared[index] {
                    let _ = self.text[index].render(&self.atlas, &self.viewport, &mut pass);
                }
                quads(&mut pass);
                pass.draw(front, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.atlas.trim();
    }
}

/// sRGB bytes to the linear floats an sRGB render target expects.
pub fn linear(rgb: stet_core::theme::Rgb, alpha: f32) -> [f32; 4] {
    let channel = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    [channel(rgb.0), channel(rgb.1), channel(rgb.2), alpha]
}

fn texture_bind(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> wgpu::BindGroup {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}
