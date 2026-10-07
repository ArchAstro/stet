# md

A minimal markdown writer in the spirit of iA Writer. Rust core, GPU-rendered,
vim keys, linked notes, review suggestions, ArchDev themes.

```sh
cargo run --release -- examples/tour.md
```

Started from a terminal, `md file.md` opens its window as a separate process
and hands the prompt straight back. `md --wait file.md` (or `-f`) stays attached
until the window closes, which is what `$EDITOR` and `git commit` need.

If md is already running, `md other.md` opens the file there as a tab in a
few milliseconds instead of starting again (`-n` forces a separate window).

Press **Cmd-/** (Ctrl-/ elsewhere, or F1) for the menu: every command with its
shortcut, markdown syntax you can insert, and the vim keys. Type to filter.

## Layout

| Crate | Owns | Platform code |
|---|---|---|
| `crates/md-core` | buffer + undo, vim and standard keys, markdown analysis, syntax highlighting, suggestions, links, tabs, menu and file-browser state, themes, config, file I/O, crash recovery | none |
| `crates/md-app` | window (`winit`), renderer (`wgpu` + `glyphon`), text layout, fonts, images, clipboard, dialogs | all of it |

The core takes `KeyEvent`s, text and mouse positions, and emits `Effect`s for
what only a shell can do (display-line motion, dialogs, opening URLs, quitting).

## Keys

Vim is on by default (`:novim` or `vim = false` for a standard text field).

1. **Motions** — `h j k l w b e W B E ge 0 ^ $ gg G f F t T ; , { } ( ) % n N * #`, marks, counts.
   Bare `j`/`k` move by wrapped display line; with a count they move by logical line.
2. **Operators** — `d c y > < g~ gu gU` with motions, counts and text objects
   (`iw aw ip ap is as i" a" i( a( i[ i{ i<` …); `x X s S D C Y J gJ r ~ p P u Ctrl-r .`
3. **Insert** — `i a I A o O`, with counts (`3ix`, `2o`).
4. **Visual** — `v`, `V`, and block `Ctrl-v` (`I`, `A`, `c`, `d`, `y`, `r`, `~ u U`, `> <`); `o` swaps ends.
5. **Registers and macros** — `"a`–`"z`, `"A` append, `"0`, `"_`, `"+`; the unnamed register is the system clipboard. `qa … q` records, `@a` and `@@` replay.
6. **Search** — `/` and `?` take regular expressions (smart case, live highlight; an invalid pattern is searched literally). `:s/a/b/g` and `:%s/(\w+)@(\w+)/\2 at \1/g` support `\1`…`\9` and `&`.
7. **Command line** — `:w :q :wq :wa :qa :e :tabnew :theme :font :monofont :files :sidebar :backlinks :recover :help :<line>`; Tab completes theme and font names.
8. **Markdown** — Enter continues lists, tasks and quotes; Tab/Shift-Tab indent items; `gt` toggles a task.

Shortcuts use Cmd on macOS and Ctrl elsewhere (in vim mode on Ctrl platforms,
vim keeps its own Ctrl chords):

| Do | Key |
|---|---|
| Menu (commands, markdown, vim keys) | `/`, or F1, or click `menu` in the status line |
| Go to file | `P` |
| File browser | `\` |
| New tab, close tab | `T` or `N`, `W` |
| Next / previous tab, tab 1–9 | `Shift-]` / `Shift-[` (vim `]t` / `[t`), `1`–`9` |
| Follow link, back, forward | `Enter` (vim `gf` or Enter), `[`, `]` (vim `Ctrl-o`, `Ctrl-i`) |
| Notes linking here | `Shift-L` |
| Save, save as, open | `S`, `Shift-S`, `O` |
| Find, next, previous | `F`, `G`, `Shift-G` |
| Bold, italic, link | `B`, `I`, `K` |
| Theme picker, focus mode, full screen | `Shift-T`, `Shift-D`, `Shift-F` or F11 |
| Zoom | `+`, `-`, `0` |

## Right-click and the actions menu

Right-click anything, or press `K` (also `gm`, Cmd/Ctrl-`.`, Shift-F10 or the
menu key) for the actions that apply at the cursor:

1. On a suggestion: accept (`a`) or reject (`r`) it.
2. On a link: follow it, open it in a new tab, copy it.
3. On a selection: cut, copy, paste, bold, italic, make a link.
4. On a task line: toggle it. Always: paste, suggestion mode, all commands.
5. In the file browser and on tabs: open, open in a new tab, copy path, close.

`j`/`k` or the arrows move, Enter or `l` runs, Esc or `q` closes, and each
item's letter (shown on the right) runs it directly.

## Your own key bindings

Any key or key sequence can be bound, per mode, in `config.toml`. Yours win
over the built-in ones, shortcuts included:

```toml
[keys.normal]
"<Space>w" = ":w"          # an editor command
"H" = "^"                  # other keys (built-in meaning, not remapped again)
"gt" = "<Nop>"             # switch a built-in off
"K" = ":agent"

[keys.insert]
"jk" = "<Esc>"             # typed as you go; taken back when the sequence completes

[keys.visual]
"s" = "d"

[keys.all]                 # every mode
"<D-j>" = ":tabnext"       # D = Cmd, C = Ctrl, M = Alt, S = Shift
```

Every action has a command to bind (the menu lists them): `:w :q :tabnew
:tabnext :files :sidebar :follow :back :forward :backlinks :actions :agent
:suggest :accept :reject :acceptall :rejectall :nextsuggestion :bold :italic
:link :task :theme :font :focus :typewriter :zoom in :fullscreen :help`.

## Assistants

Other programs work in your documents through `md ctl` while you type. The
included skill (`skill/md/SKILL.md`) teaches Claude Code to use it.

| Command | Does |
|---|---|
| `md ctl sessions` | lists open documents |
| `md ctl read [--doc D] [--lines A-B]` | the live text, cursor, selection, suggestions |
| `md ctl suggest --old T --new T` | proposes a change as a suggestion you accept or reject |
| `md ctl edit ...` | changes the text directly (one undo step) |
| `md ctl wait --name Claude` | connects an assistant and waits for your message |
| `md ctl say --text T` | shows a line in the status bar |

1. **Your flow is left alone.** An edit never moves your cursor, changes mode, steals focus or saves; it lands as its own undo step, in background tabs too.
2. **Edits follow your typing.** A target is either the exact text to replace, or a range read at an earlier revision that is carried forward through everything typed since (the transform half of operational transformation, with the window as the single authority). If you changed that same text, the edit is refused rather than misplaced.
3. **You can talk back.** While an assistant waits, the status bar shows `● Claude`. Press Cmd/Ctrl-Shift-A (or `ga`, or `:agent <message>`) to send it a request; a selection travels with it ("make this punchier"), and without one it is a general request ("make a diagram and insert it"). `◌ Claude working` shows until it comes back for the next one.
4. The socket is `~/.config/md/md.sock`, and only your user can connect to it. macOS and Linux only for now.

## Linked notes

A folder of markdown files works as a knowledge base.

1. `[[Note]]` opens `Note.md` from anywhere in the workspace (nearest first); `[[Note#Heading]]` lands on a heading; `[[Note|shown text]]` is allowed. Relative links `[text](other.md#heading)` work the same way.
2. A link to a note that does not exist opens an empty one beside the current file; saving creates it.
3. Typing `[[` offers the notes you can link.
4. Following a link replaces the document in place when it has no unsaved changes, and opens a tab otherwise. Back and forward retrace your path across files.
5. `:backlinks` lists every note that links to the current one.
6. The workspace is the nearest parent folder containing `.git`, `.obsidian` or `.md-root`, else the document's folder.
7. URLs, images and other files open in their own apps.

## File browser

Hidden until you press Cmd/Ctrl-`\` (or `:sidebar`). It shows the document's
folder; `j`/`k` or arrows move, Enter or `l` opens a file or folder, `h`
closes one, `t` opens in a new tab, `-` widens to the parent folder, `r`
re-reads, Esc returns to the text. Clicking works too. `sidebar = true` shows
it at startup.

## Code

Fenced code blocks and front matter are colored by language using Sublime
Text grammars (`syntect` + `two-face`, about 200 languages; pure Rust). Colors
come from the theme. `highlight = false` turns it off.

## Suggestions

Suggestions are CriticMarkup in the document text, following ArchDev's
plan-collab convention, so they survive any tool and merge like ordinary edits:

```text
{++insert++}{>>id:s_0123abcd by:Calvin<<}
{--delete--}{>>id:s_0123abce by:Calvin<<}
{~~old~>new~~}{>>id:s_0123abcf by:Calvin<<}
```

| Do | Key |
|---|---|
| Toggle suggestion mode | `:suggest` or Cmd/Ctrl-Shift-E |
| Next / previous suggestion | `]s` / `[s` |
| Accept / reject at cursor | `gsa` / `gsr`, or Cmd/Ctrl-Shift-Y / -N |
| Resolve everything | `:acceptall` / `:rejectall` |
| Set your name | `:author <name>` or `author` in the config |

In suggestion mode typing extends one insertion, repeated deletes grow one
deletion, and `c` produces a replacement. Text the convention cannot represent
(reserved tokens, code fences, another author's suggestion) is refused with a
message rather than written wrongly.

## Fonts

Every installed font is available.

1. `:font` and `:monofont` open a picker that previews as you move; `:font Georgia` sets one directly (Tab completes). `md --fonts` lists the families.
2. Generic names follow the platform: `system-ui` (San Francisco, Segoe UI, …), `serif`, `sans-serif`, `monospace`.
3. Proportional fonts work for prose; code, tables and the status line use the code font.
4. Any `.ttf`/`.otf` in `~/.config/md/fonts/` is loaded as well.

## Images

Shown under the line that references them: local files, `data:` URIs and
`http(s)` URLs (PNG, JPEG, GIF, WebP, BMP). Remote images are fetched in the
background and cached; `remote_images = false` keeps a document from
contacting servers. Dropping an image on the window inserts a link to it.

## Never losing text

1. Saves are atomic (temp file, fsync, rename) and refuse to overwrite a file that changed on disk.
2. Unsaved text is snapshotted to `~/.config/md/recovery/` shortly after you stop typing. After a crash, reopening the file restores it as an undoable edit (`u` returns to the saved version); untitled drafts come back as tabs.
3. An orderly quit, or saving, removes the snapshots.

## Configuration

`~/.config/md/config.toml` (`%APPDATA%\md` on Windows; `MD_CONFIG_DIR` overrides):

```toml
theme = "paper"        # latte frappe macchiato mocha gruvbox-light everforest
                       # everforest-light dracula solaris-light solaris-dark
                       # paper ristretto nord
vim = true
author = "Calvin"
font_size = 17
line_height = 1.55
line_width = 72        # characters
prose_font = ["iA Writer Quattro S", "monospace"]
mono_font = ["iA Writer Mono S", "monospace"]
ui_font = ["system-ui"]
visual_line_motion = true
regex_search = true
highlight = true
link_completion = true
remote_images = true
sidebar = false
focus = false
typewriter = false
```

The theme, fonts, zoom and window size you choose while running are remembered
in `state.toml` beside it. Editing `config.toml` afterwards makes the file win
again.

- **Themes** — drop `~/.config/md/themes/<name>.toml`; it inherits a built-in
  and overrides palette tokens or editor roles (including `syntax_keyword`,
  `syntax_string`, … and `panel`):

  ```toml
  label = "Mine"
  base = "nord"
  [palette]
  blue = "#00aaff"
  [editor]
  cursor = "#ff5555"
  ```

## Speed

Measured with `--bench` on an M-series Mac (release build):

| Document | Keystroke to frame built | Full analysis |
|---|---|---|
| 1.5 KB | 0.07 ms | 0.01 ms |
| 0.9 MB of prose, 21k lines | 0.2 ms | 3.6 ms |
| 0.9 MB with 1,800 suggestions | 0.6 ms | 4.8 ms |

1. **Analysis is incremental.** An edit re-parses only the top-level blocks around it and splices the result into the previous analysis. Where markdown lets distant text decide how a block reads (reference and footnote definitions, unclosed fences, metadata blocks), it falls back to a full pass. A randomized test checks that the incremental result equals a full one after every edit (`MD_FUZZ=50000 cargo test --release -p md-core incremental_analysis` runs three million of them). Below 16 KB the whole document is simply re-parsed.
2. **Layout is cached per line**, keyed by content, so a frame after a keystroke shapes one line.
3. **Code highlighting restarts at the edited line** and stops when the parser state converges with the previous run.
4. **Nothing slow runs on the typing thread.** Crash-recovery snapshots (a synced write, 4–5 ms) happen on a worker.
5. **A second launch does not start anything**: it hands the file to the running window.
6. **Startup overlaps work with the OS creating the window**: fonts are scanned and the first screen shaped on another thread. `MD_TIMING=1 md -f file.md` prints each step.
7. **Frames are presented as soon as they are drawn** rather than queued behind the display refresh.

`MD_TRACE_PARSE=1` reports each edit that needed a full analysis, and why.

## Checks

```sh
cargo test --workspace
cargo run --release -- --bench <file>                              # hot-path timings
cargo run --release -- --screenshot out.png --keys 'ggvj' <file>   # off-screen render
```

`--drive` reads scripted input from stdin and feeds it through the running
window's own event handlers, for end-to-end checks without touching the mouse:

```text
keys :theme nord<CR>
text typed as if by the IME
click 300 200          # points; add a count, or cmd / shift
drag 100 100 400 300
scroll 300 300 240
resize 900 700
shot /tmp/frame.png    # what the window shows now
quit
```

Building without HTTPS (`--no-default-features`) drops remote images and the
only dependency that needs a C compiler for the target.
