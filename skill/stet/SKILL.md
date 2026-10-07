---
name: stet
description: Work with the user inside their Stet markdown editor - see what they have open, read the live (unsaved) text, propose edits as suggestions they accept or reject, insert text or images at their cursor, and take requests they send from the editor. Also covers installing Stet and this skill. Use when the user asks for help writing or editing a document they have open in Stet, says "my editor/draft/doc", asks you to listen for requests from Stet, or asks to install or set up Stet.
---

# Working in the user's Stet editor

`stet ctl` talks to the running Stet window. Every reply is JSON with `"ok"`.
The user is usually typing while you work: nothing you do moves their cursor,
steals focus, or saves the file.

## 0. Is Stet installed?

```sh
command -v stet && stet ctl sessions
```

- `stet` found, and the reply lists sessions: go to step 1.
- `stet` found, but the reply says Stet is not running: ask the user to open
  their document (`stet notes.md`). Do not launch it for them unless asked.
- `stet` not found: offer to install it, and install only when they agree.

### Installing Stet and this skill

```sh
curl -fsSL https://raw.githubusercontent.com/ArchAstro/stet/main/install.sh | sh
```

This downloads the latest release for macOS or Linux (x64), verifies its
checksum, puts `stet` in `~/.local/bin` and this skill in `~/.claude/skills/stet`
(other agents: copy `SKILL.md` to wherever your skills live). If the download
is refused because the repository is private, and `gh` is signed in:

```sh
gh api repos/ArchAstro/stet/contents/install.sh -H "Accept: application/vnd.github.raw" | sh
```

If there is no prebuilt binary for the machine, build from source (needs
Rust; if `cargo` is missing, point the user to https://rustup.rs rather than
installing a toolchain yourself):

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git https://github.com/ArchAstro/stet --locked stet-app
```

Check it worked with `stet --version`, then have the user open a document.
Running the installer again updates Stet. To remove it: delete `~/.local/bin/stet`
and `~/.claude/skills/stet`.

`stet ctl` (everything below) works on macOS and Linux.

## 1. See what is open

```sh
stet ctl sessions
```

Each session has a `tab` number, `path`, `title`, `active` (the one in
front), `unsaved`, `lines`, `words`, `revision` and the `cursor`. If the
reply says Stet is not running, tell the user; do not start it yourself.

## 2. Read before you edit

```sh
stet ctl read                      # the document in front, whole
stet ctl read --doc notes.md --lines 40-80
```

- Always read through `stet ctl read`, never from disk: the buffer has text
  that is not saved yet.
- The reply has `text`, `revision`, `cursor`, the user's `selection` (if
  any) and the open `suggestions`.
- `--doc` takes a tab number, a file name or a path. Without it you get the
  document in front.

## 3. Propose edits (the default)

```sh
stet ctl suggest --old "the exact text to replace" --new "the replacement"
```

- `suggest` writes a CriticMarkup suggestion under your name. The user sees
  old and new side by side and accepts or rejects it. Use this unless they
  explicitly asked you to change the text directly.
- `--old` must match the live text exactly and occur once. Quote enough
  surrounding words to make it unique, or add `--occurrence N`.
- Keep each suggestion small: a sentence or a paragraph, not the whole
  document. Several small suggestions are easier to review than one large.
- Do not include a trailing newline in `--old` or `--new` unless you mean to
  change the line break.
- For text with quotes or newlines, write it to a file and pass `@file`:
  `--new @/tmp/new.txt` (or `-` for stdin).
- If the reply is an error (text not found, changed since read, cannot be
  written inside a suggestion), read again and retry once. Never fall back
  to editing the file on disk.

Other targets, for `suggest` and `edit` alike:

```sh
stet ctl suggest --at cursor --text "inserted where the user's cursor is"
stet ctl suggest --at end --text $'\n## Next steps\n'
stet ctl suggest --line 12 --text $'A new paragraph before line 12.\n\n'
stet ctl suggest --range 57:210-241 --text "replacement"
```

`--range REV:START-END` replaces characters START..END as they were at
revision REV (use the `revision` and offsets from a read or from a message).
Whatever the user typed since is accounted for; if they changed that very
text, you get an error instead of a wrong edit.

## 4. Direct edits (only when asked)

```sh
stet ctl edit --old "teh" --new "the"
stet ctl edit --at cursor --text "![Diagram](diagram.png)"
```

`edit` changes the text at once (one undo step for the user). Use it when
the user asked for the change itself: "fix it", "insert it", "just do it".

### Making an image and inserting it

1. Create the image file next to the document (the directory of the
   session's `path`), with a descriptive name.
2. Insert a relative link where they asked, usually the cursor:
   `stet ctl edit --at cursor --text "![What it shows](name.png)"`.
   Stet displays it under that line immediately.

## 5. Taking requests from inside Stet

The user can message you without leaving the editor (Cmd/Ctrl-Shift-A, or
`ga` in vim mode). To receive those, wait:

```sh
stet ctl wait --name Claude
```

- Run it in the background. It blocks until the user sends a message, prints
  it, and exits; Stet shows you as connected while it waits.
- The message JSON has `message` (what they typed), `session` (which
  document), `revision`, `cursor`, `line_text`, and `selection` with `start`,
  `end` and `text` when they had text selected. A request with a selection
  is about that text: answer with
  `stet ctl suggest --range <revision>:<start>-<end> --text "..."` (leave out
  a final newline in the selection if you are not replacing it).
- Without a selection it is a general request ("draft an intro", "make a
  diagram of this and insert it"): read what you need, then act at the
  cursor or where they said.
- When you have acted, tell them in one line:
  `stet ctl say --text "Suggested a tighter opening - accept or reject it."`
- Then run `stet ctl wait` again to stay connected. Stop when the user tells
  you to, or when Stet is no longer running.

## 6. Operating the window

For the rare request that is about the editor rather than the text ("switch
to the dark theme", "open the file browser", "show me what you see"):

```sh
stet ctl command "theme stet-ink"     # any editor command, as typed after `:`
stet ctl keys "<D-/>"                 # keys in vim notation; <D-…> is Cmd, <C-…> Ctrl
stet ctl type "some text"             # typed as the keyboard would
stet ctl click 320 200                # a click at a point in the window; add `right` for the menu
stet ctl shot /tmp/stet.png           # a picture of what the window shows right now
```

- Each reply says what state the editor is in (`mode`, whether a `menu` or
  `prompt` is open, the status `message`). Use `shot` when you need to see it.
- These act exactly like the user's own keyboard and mouse, so they do move
  the cursor and change modes. Use them only when asked, never to edit text
  (that is what `suggest` and `edit` are for), and leave the editor as you
  found it: close menus you opened, return to the tab they were on.

## Rules

1. Suggest by default; edit directly only on request.
2. Never write the document's file on disk, and never save it for them.
3. Do not accept or reject suggestions, yours or theirs.
4. One request, one focused change. Say what you did in a line.
5. If something fails twice, tell the user what happened instead of working
   around it.
