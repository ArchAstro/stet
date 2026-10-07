//! Vim normal and visual modes: key grammar, motions, operators, registers,
//! dot-repeat and search.

use super::motion::{self, Matcher};
use super::{CmdKind, CmdLine, Editor, Effect, Input, Mode, ScrollTo};
use crate::input::{Key, KeyEvent};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Clone, Default)]
pub(super) struct Register {
    text: String,
    linewise: bool,
}

pub(super) struct Search {
    pub(super) matcher: Matcher,
    pub(super) forward: bool,
}

/// What to do when the current insert ends: `3ix`, `3o`, and block `I`/`A`.
struct InsertRepeat {
    count: usize,
    inputs: Vec<Input>,
    /// `o`/`O`: each repeat opens its own line (true = below).
    open: Option<bool>,
    block: Option<BlockInsert>,
}

struct BlockInsert {
    first: usize,
    last: usize,
    col: usize,
    /// Where typing began on the first line.
    start: usize,
}

#[derive(Default)]
pub(super) struct State {
    pending: Vec<KeyEvent>,
    registers: HashMap<char, Register>,
    last_find: Option<(char, bool, bool)>,
    pub(super) search: Option<Search>,
    /// The pattern being typed at the `/` prompt.
    pub(super) preview: Option<Matcher>,
    pub(super) highlight: bool,
    marks: HashMap<char, usize>,
    last_change: Vec<Input>,
    recording: Option<(Vec<Input>, u64)>,
    replaying: bool,
    suppress: bool,
    macros: HashMap<char, Vec<Input>>,
    pub(super) macro_rec: Option<(char, Vec<Input>)>,
    last_macro: Option<char>,
    /// Macro nesting depth; display-line motion is off while one plays.
    playing: u32,
    insert_repeat: Option<InsertRepeat>,
}

impl State {
    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }
    pub(super) fn clear_pending(&mut self) {
        self.pending.clear();
    }
    pub(super) fn is_replaying(&self) -> bool {
        self.replaying || self.playing > 0
    }
    pub(super) fn suppress_record(&mut self) {
        self.suppress = true;
    }
    pub(super) fn matcher(&self, typing: bool) -> Option<&Matcher> {
        if typing {
            self.preview.as_ref()
        } else {
            self.search.as_ref().filter(|_| self.highlight).map(|search| &search.matcher)
        }
    }
    /// Remembers typed input while `3i…` or a block insert is open.
    pub(super) fn collect_insert(&mut self, input: Input) {
        if let Some(repeat) = &mut self.insert_repeat {
            repeat.inputs.push(input);
        }
    }
    pub(super) fn pending_display(&self) -> String {
        self.pending
            .iter()
            .map(|event| match event.key {
                Key::Char(c) if event.mods.ctrl => format!("^{}", c.to_ascii_uppercase()),
                Key::Char(c) => c.to_string(),
                _ => String::new(),
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    DisplayUp,
    DisplayDown,
    WordFwd(bool),
    WordBack(bool),
    WordEnd(bool),
    WordEndBack(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    Column,
    FirstLine,
    LastLine,
    Find { ch: char, forward: bool, till: bool },
    RepeatFind { reverse: bool },
    ParaFwd,
    ParaBack,
    SentFwd,
    SentBack,
    MatchPair,
    SearchNext { reverse: bool },
    StarSearch { forward: bool },
    Mark(char),
    /// Fraction of a screen.
    Page(f32),
    Suggestion { forward: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Exclusive,
    Inclusive,
    Line,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Dedent,
    ToggleCase,
    Lower,
    Upper,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Motion(Motion),
    Lines,
    Object(bool, char),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InsertAt {
    Cursor,
    After,
    FirstNonBlank,
    LineEnd,
    Below,
    Above,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Move(Motion),
    Operate(Op, Target, Option<usize>),
    VisualOp(Op),
    SelectObject(bool, char),
    Insert(InsertAt),
    Put { before: bool },
    Undo,
    Redo,
    Repeat,
    Visual(Mode),
    /// `I`/`A` on a selection (true = append).
    VisualInsert(bool),
    VisualReplace(char),
    SwapEnds,
    Join { spaces: bool },
    Tilde,
    Replace(char),
    CmdLine(CmdKind),
    Scroll(ScrollTo),
    SaveQuit,
    QuitForce,
    SetMark(char),
    Resolve { accept: bool },
    ToggleTask,
    ContextMenu,
    AgentPrompt,
    RecordMacro(char),
    PlayMacro(char),
    FollowLink,
    /// Enter: follow a link under the cursor, else move down.
    Enter,
    Jump { back: bool },
    Tab(isize),
    Escape,
}

struct Cmd {
    register: Option<char>,
    count: Option<usize>,
    action: Action,
}

enum Parse {
    Incomplete,
    Invalid,
}

struct Reader<'a> {
    keys: &'a [KeyEvent],
    at: usize,
}

impl Reader<'_> {
    fn next(&mut self) -> Result<KeyEvent, Parse> {
        let event = self.keys.get(self.at).copied().ok_or(Parse::Incomplete)?;
        self.at += 1;
        Ok(event)
    }

    fn peek_char(&self) -> Option<char> {
        self.keys.get(self.at).and_then(KeyEvent::plain_char)
    }

    /// The next key as a literal character argument (`f`, `r`, `m`, `"`).
    fn char(&mut self) -> Result<char, Parse> {
        let event = self.next()?;
        match event.key {
            Key::Enter => Ok('\n'),
            Key::Tab => Ok('\t'),
            _ => event.plain_char().ok_or(Parse::Invalid),
        }
    }

    fn count(&mut self) -> Option<usize> {
        let mut count: Option<usize> = None;
        while let Some(digit) = self.peek_char().and_then(|c| c.to_digit(10)) {
            if count.is_none() && digit == 0 {
                break;
            }
            count = Some(count.unwrap_or(0).saturating_mul(10).saturating_add(digit as usize).min(1_000_000));
            self.at += 1;
        }
        count
    }
}

fn parse(keys: &[KeyEvent], visual: bool) -> Result<Cmd, Parse> {
    let mut reader = Reader { keys, at: 0 };
    let mut register = None;
    if reader.peek_char() == Some('"') {
        reader.at += 1;
        register = Some(reader.char()?);
    }
    let count = reader.count();
    let event = reader.next()?;
    let action = parse_action(event, &mut reader, visual)?;
    Ok(Cmd { register, count, action })
}

fn parse_action(event: KeyEvent, r: &mut Reader, visual: bool) -> Result<Action, Parse> {
    use Action::*;
    if event.mods.ctrl {
        let Key::Char(c) = event.key else {
            return motion_of(event, r).map(Move);
        };
        return Ok(match c {
            'r' => Redo,
            'v' => Visual(Mode::VisualBlock),
            'o' => Jump { back: true },
            'i' => Jump { back: false },
            'd' => Move(Motion::Page(0.5)),
            'u' => Move(Motion::Page(-0.5)),
            'f' => Move(Motion::Page(1.0)),
            'b' => Move(Motion::Page(-1.0)),
            'h' => Move(Motion::Left),
            'j' | 'n' => Move(Motion::Down),
            'p' => Move(Motion::Up),
            'c' | '[' | 'l' => Escape,
            _ => return Err(Parse::Invalid),
        });
    }
    let c = match event.key {
        Key::Esc => return Ok(Escape),
        Key::Enter if !visual => return Ok(Enter),
        Key::Char(c) if event.plain_char().is_some() => c,
        Key::Char(_) => return Err(Parse::Invalid),
        _ => return motion_of(event, r).map(Move),
    };

    let mut case_char = None;
    let op = match c {
        'd' => Some(Op::Delete),
        'c' => Some(Op::Change),
        'y' => Some(Op::Yank),
        '>' => Some(Op::Indent),
        '<' => Some(Op::Dedent),
        'g' => {
            let next = r.peek_char().ok_or(if r.at < r.keys.len() { Parse::Invalid } else { Parse::Incomplete })?;
            let op = match next {
                '~' => Some(Op::ToggleCase),
                'u' => Some(Op::Lower),
                'U' => Some(Op::Upper),
                _ => None,
            };
            if op.is_some() {
                r.at += 1;
                case_char = Some(next);
            }
            op
        }
        _ => None,
    };
    if let Some(op) = op {
        if visual {
            return Ok(VisualOp(op));
        }
        let count = r.count();
        let next = r.next()?;
        let target = match next.plain_char() {
            Some(n) if case_char.is_none() && n == c => Target::Lines,
            Some(n) if case_char == Some(n) => Target::Lines,
            Some(n @ ('i' | 'a')) => Target::Object(n == 'a', r.char()?),
            _ => Target::Motion(motion_of(next, r)?),
        };
        return Ok(Operate(op, target, count));
    }

    if visual {
        let action = match c {
            'x' | 'X' | 'D' => Some(VisualOp(Op::Delete)),
            's' | 'S' | 'C' => Some(VisualOp(Op::Change)),
            'Y' => Some(VisualOp(Op::Yank)),
            '~' => Some(VisualOp(Op::ToggleCase)),
            'u' => Some(VisualOp(Op::Lower)),
            'U' => Some(VisualOp(Op::Upper)),
            'o' | 'O' => Some(SwapEnds),
            'I' | 'A' => Some(VisualInsert(c == 'A')),
            'r' => Some(VisualReplace(r.char()?)),
            'i' | 'a' => Some(SelectObject(c == 'a', r.char()?)),
            _ => None,
        };
        if let Some(action) = action {
            return Ok(action);
        }
    }

    let del = |motion| Operate(Op::Delete, Target::Motion(motion), None);
    let change = |target| Operate(Op::Change, target, None);
    Ok(match c {
        'i' => Insert(InsertAt::Cursor),
        'a' => Insert(InsertAt::After),
        'I' => Insert(InsertAt::FirstNonBlank),
        'A' => Insert(InsertAt::LineEnd),
        'o' => Insert(InsertAt::Below),
        'O' => Insert(InsertAt::Above),
        'x' => del(Motion::Right),
        'X' => del(Motion::Left),
        'D' => del(Motion::LineEnd),
        'C' => change(Target::Motion(Motion::LineEnd)),
        's' => change(Target::Motion(Motion::Right)),
        'S' => change(Target::Lines),
        'Y' => Operate(Op::Yank, Target::Lines, None),
        'p' => Put { before: false },
        'P' => Put { before: true },
        'u' => Undo,
        '.' => Repeat,
        'v' => Visual(Mode::Visual),
        'V' => Visual(Mode::VisualLine),
        'q' => RecordMacro(r.char()?),
        'K' => ContextMenu,
        '@' => PlayMacro(r.char()?),
        ']' | '[' if r.peek_char() == Some('t') => {
            r.at += 1;
            Tab(if c == ']' { 1 } else { -1 })
        }
        'J' => Join { spaces: true },
        '~' => Tilde,
        'r' => Replace(r.char()?),
        ':' => CmdLine(CmdKind::Command),
        '/' => CmdLine(CmdKind::SearchForward),
        '?' => CmdLine(CmdKind::SearchBackward),
        'z' => match r.char()? {
            'z' | '.' => Scroll(ScrollTo::Center),
            't' => Scroll(ScrollTo::Top),
            'b' => Scroll(ScrollTo::Bottom),
            _ => return Err(Parse::Invalid),
        },
        'Z' => match r.char()? {
            'Z' => SaveQuit,
            'Q' => QuitForce,
            _ => return Err(Parse::Invalid),
        },
        'm' => SetMark(r.char()?),
        'g' => match r.peek_char() {
            Some('J') => {
                r.at += 1;
                Join { spaces: false }
            }
            Some('t') => {
                r.at += 1;
                ToggleTask
            }
            Some('f' | 'x' | 'd') => {
                r.at += 1;
                FollowLink
            }
            Some('m') => {
                r.at += 1;
                ContextMenu
            }
            Some('a') => {
                r.at += 1;
                AgentPrompt
            }
            Some('s') => {
                r.at += 1;
                match r.char()? {
                    'a' => Resolve { accept: true },
                    'r' => Resolve { accept: false },
                    _ => return Err(Parse::Invalid),
                }
            }
            _ => Move(motion_of(event, r)?),
        },
        _ => Move(motion_of(event, r)?),
    })
}

fn motion_of(event: KeyEvent, r: &mut Reader) -> Result<Motion, Parse> {
    use Motion::*;
    Ok(match event.key {
        Key::Left | Key::Backspace => Left,
        Key::Right => Right,
        Key::Up => Up,
        Key::Down | Key::Enter => Down,
        Key::Home => LineStart,
        Key::End => LineEnd,
        Key::PageUp => Page(-1.0),
        Key::PageDown => Page(1.0),
        Key::Char(c) if event.plain_char().is_some() => match c {
            'h' => Left,
            'l' | ' ' => Right,
            'j' | '+' => Down,
            'k' | '-' => Up,
            'w' => WordFwd(false),
            'W' => WordFwd(true),
            'b' => WordBack(false),
            'B' => WordBack(true),
            'e' => WordEnd(false),
            'E' => WordEnd(true),
            '0' => LineStart,
            '^' | '_' => FirstNonBlank,
            '$' => LineEnd,
            '|' => Column,
            'G' => LastLine,
            'f' => Find { ch: r.char()?, forward: true, till: false },
            'F' => Find { ch: r.char()?, forward: false, till: false },
            't' => Find { ch: r.char()?, forward: true, till: true },
            'T' => Find { ch: r.char()?, forward: false, till: true },
            ';' => RepeatFind { reverse: false },
            ',' => RepeatFind { reverse: true },
            '}' => ParaFwd,
            '{' => ParaBack,
            ')' => SentFwd,
            '(' => SentBack,
            '%' => MatchPair,
            'n' => SearchNext { reverse: false },
            'N' => SearchNext { reverse: true },
            '*' => StarSearch { forward: true },
            '#' => StarSearch { forward: false },
            '`' | '\'' => Mark(r.char()?),
            'g' => match r.char()? {
                'g' => FirstLine,
                'e' => WordEndBack(false),
                'E' => WordEndBack(true),
                'j' => DisplayDown,
                'k' => DisplayUp,
                '0' => LineStart,
                '$' => LineEnd,
                '^' | '_' => FirstNonBlank,
                _ => return Err(Parse::Invalid),
            },
            ']' | '[' => match r.char()? {
                's' => Suggestion { forward: c == ']' },
                _ => return Err(Parse::Invalid),
            },
            _ => return Err(Parse::Invalid),
        },
        _ => return Err(Parse::Invalid),
    })
}

enum OpRange {
    Chars(Range<usize>),
    /// Inclusive line range.
    Lines(usize, usize),
}

impl Editor {
    // ----- dot-repeat recording ------------------------------------------

    fn at_rest(&self) -> bool {
        self.mode == Mode::Normal && self.vim.pending.is_empty() && self.cmdline.is_none()
    }

    pub(super) fn vim_record_start(&mut self) {
        if !self.vim.replaying && self.vim.recording.is_none() && self.at_rest() {
            self.vim.recording = Some((Vec::new(), self.buf.revision()));
            self.vim.suppress = false;
        }
    }

    pub(super) fn vim_record(&mut self, input: Input) {
        if let Some((inputs, _)) = &mut self.vim.recording {
            inputs.push(input);
        }
    }

    pub(super) fn vim_record_end(&mut self) {
        if self.vim.replaying || !self.at_rest() {
            return;
        }
        if let Some((inputs, revision)) = self.vim.recording.take()
            && revision != self.buf.revision() && !self.vim.suppress {
                self.vim.last_change = inputs;
            }
    }

    // ----- key handling ---------------------------------------------------

    pub(super) fn vim_key(&mut self, event: KeyEvent) {
        if self.vim.pending.is_empty() && event.plain_char() == Some('q')
            && let Some((register, mut inputs)) = self.vim.macro_rec.take() {
                // Drop the `q` that ended the recording.
                inputs.pop();
                self.vim.macros.insert(register, inputs);
                return;
            }
        self.vim.pending.push(event);
        let visual = self.mode != Mode::Normal;
        match parse(&self.vim.pending, visual) {
            Err(Parse::Incomplete) => {}
            Err(Parse::Invalid) => self.vim.pending.clear(),
            Ok(cmd) => {
                self.vim.pending.clear();
                self.execute(cmd);
            }
        }
    }

    fn execute(&mut self, cmd: Cmd) {
        let Cmd { register, count, action } = cmd;
        let n = count.unwrap_or(1);
        let line = self.buf.line_of(self.cursor);
        match action {
            Action::Move(motion) => {
                if let Some((pos, _)) = self.motion_target(motion, count, false) {
                    self.cursor = pos;
                }
            }
            Action::Operate(op, target, inner) => {
                let count = match (count, inner) {
                    (None, None) => None,
                    (outer, inner) => Some(outer.unwrap_or(1).saturating_mul(inner.unwrap_or(1))),
                };
                self.operate(op, target, count, register);
            }
            Action::VisualOp(op) if self.mode == Mode::VisualBlock => self.block_op(op, register),
            Action::VisualOp(op) => {
                let Some(selection) = self.selection() else { return };
                let low = selection.start;
                let range = if self.mode == Mode::VisualLine {
                    OpRange::Lines(self.buf.line_of(low), self.buf.line_of(self.anchor.unwrap_or(low).max(self.cursor)))
                } else {
                    OpRange::Chars(selection)
                };
                self.anchor = None;
                self.mode = Mode::Normal;
                self.cursor = low;
                self.apply_op(op, range, register, low);
            }
            Action::SelectObject(around, ch) => {
                let Some(object) = motion::text_object(&self.buf, self.cursor, around, ch) else { return };
                if object.linewise {
                    self.mode = Mode::VisualLine;
                    self.anchor = Some(self.buf.line_start(object.range.start));
                    self.cursor = self.buf.line_start(object.range.end);
                } else if !object.range.is_empty() {
                    self.mode = Mode::Visual;
                    self.anchor = Some(object.range.start);
                    self.cursor = object.range.end - 1;
                }
            }
            Action::Insert(at) => {
                self.anchor = None;
                self.open_group();
                let (start, end) = (self.buf.line_start(line), self.buf.line_end(line));
                self.mode = Mode::Insert;
                self.cursor = match at {
                    InsertAt::Cursor => self.cursor,
                    InsertAt::After if start == end => self.cursor,
                    InsertAt::After => motion::next_grapheme(&self.buf, self.cursor),
                    InsertAt::FirstNonBlank => motion::first_non_blank(&self.buf, line),
                    InsertAt::LineEnd => end,
                    InsertAt::Below => self.edit(end..end, "\n").end,
                    // `start` stays before the newline, inside suggestion markup too.
                    InsertAt::Above => self.edit(start..start, "\n").start,
                };
                self.vim.insert_repeat = (n > 1).then(|| InsertRepeat {
                    count: n,
                    inputs: Vec::new(),
                    open: match at {
                        InsertAt::Below => Some(true),
                        InsertAt::Above => Some(false),
                        _ => None,
                    },
                    block: None,
                });
            }
            Action::VisualInsert(append) => {
                let Some(selection) = self.selection() else { return };
                let block = (self.mode == Mode::VisualBlock).then(|| self.block_rect());
                let linewise = self.mode == Mode::VisualLine;
                self.anchor = None;
                self.open_group();
                self.mode = Mode::Insert;
                if let Some((first, last, left, right)) = block {
                    let col = if append { right + 1 } else { left };
                    self.cursor = self.buf.line_start(first) + col.min(self.buf.line_len(first));
                    self.vim.insert_repeat = Some(InsertRepeat {
                        count: 1,
                        inputs: Vec::new(),
                        open: None,
                        block: Some(BlockInsert { first, last, col, start: self.cursor }),
                    });
                } else if append {
                    let end = selection.end.max(1) - usize::from(linewise);
                    self.cursor = end.max(selection.start);
                } else {
                    self.cursor = selection.start;
                }
            }
            Action::VisualReplace(ch) => {
                let ranges = self.selection_ranges();
                let Some(low) = ranges.first().map(|range| range.start) else { return };
                self.anchor = None;
                self.mode = Mode::Normal;
                for range in ranges.into_iter().rev() {
                    let old = self.buf.slice(range.clone());
                    let new: String = old.chars().map(|c| if c == '\n' { c } else { ch }).collect();
                    if new != old {
                        self.edit(range, &new);
                    }
                }
                if !self.suggesting {
                    self.cursor = low;
                }
            }
            Action::Put { before } => self.put(register, before, n),
            Action::Undo => self.undo(n),
            Action::Redo => self.redo(n),
            Action::Repeat => self.repeat(n),
            Action::Visual(target) => {
                if self.mode == target {
                    self.mode = Mode::Normal;
                    self.anchor = None;
                } else {
                    self.anchor.get_or_insert(self.cursor);
                    self.mode = target;
                }
            }
            Action::SwapEnds => {
                if let Some(anchor) = self.anchor.replace(self.cursor) {
                    self.cursor = anchor;
                }
            }
            Action::Join { spaces } => {
                let (first, joins) = match self.selection() {
                    Some(selection) => {
                        let first = self.buf.line_of(selection.start);
                        let last = self.buf.line_of(selection.end.saturating_sub(1).max(selection.start));
                        (first, (last - first).max(1))
                    }
                    None => (line, n.max(2) - 1),
                };
                self.anchor = None;
                self.mode = Mode::Normal;
                self.join_lines(first, joins, spaces);
            }
            Action::Tilde => {
                let end = (self.cursor + n).min(self.buf.line_end(line));
                let old = self.buf.slice(self.cursor..end);
                let new = toggle_case(&old);
                if new != old {
                    self.edit(self.cursor..end, &new);
                }
                if !self.suggesting {
                    self.cursor = end;
                }
            }
            Action::Replace(ch) => {
                let end = self.cursor + n;
                if end > self.buf.line_end(line) {
                    return;
                }
                let new = if ch == '\n' { "\n".to_string() } else { ch.to_string().repeat(n) };
                let pos = self.edit(self.cursor..end, &new);
                self.cursor = if ch == '\n' { pos.end } else { pos.end.saturating_sub(1).max(pos.start) };
            }
            Action::CmdLine(kind) => {
                self.anchor = None;
                self.mode = Mode::Normal;
                self.cmdline = Some(CmdLine { kind, text: String::new() });
            }
            Action::Scroll(to) => self.effects.push(Effect::Scroll(to)),
            Action::SaveQuit => self.run_command("x"),
            Action::QuitForce => self.run_command("q!"),
            Action::SetMark(mark) => {
                self.vim.marks.insert(mark, self.cursor);
            }
            Action::Resolve { accept } => self.resolve_at_cursor(accept),
            Action::ToggleTask => self.toggle_task(),
            Action::ContextMenu => self.open_context_menu(super::MenuAt::Cursor),
            Action::AgentPrompt => self.agent_prompt(),
            Action::RecordMacro(register) => self.vim.macro_rec = Some((register, Vec::new())),
            Action::PlayMacro(register) => self.play_macro(register, n),
            Action::FollowLink => self.follow_link(),
            Action::Enter => {
                if count.is_none() && self.link_at(self.cursor).is_some() {
                    self.follow_link();
                } else if let Some((pos, _)) = self.motion_target(Motion::Down, count, false) {
                    self.cursor = pos;
                }
            }
            Action::Jump { back } => self.jump(back),
            Action::Tab(delta) => self.cycle_tab(delta),
            Action::Escape => {
                if self.mode == Mode::Normal {
                    self.vim.highlight = false;
                }
                self.anchor = None;
                self.mode = Mode::Normal;
            }
        }
    }

    fn play_macro(&mut self, register: char, count: usize) {
        let register = if register == '@' { self.vim.last_macro } else { Some(register) };
        let Some(inputs) = register.and_then(|register| self.vim.macros.get(&register).cloned()) else {
            return self.error("no macro recorded there");
        };
        if self.vim.playing >= 8 {
            return;
        }
        self.vim.last_macro = register;
        self.vim.playing += 1;
        for _ in 0..count.clamp(1, 10_000) {
            for input in &inputs {
                match input {
                    Input::Key(event) => self.dispatch_key(*event),
                    Input::Text(text) => self.type_text(text),
                }
                self.after_input();
            }
        }
        self.vim.playing -= 1;
    }

    /// The register being recorded into, for the status line.
    pub fn recording_macro(&self) -> Option<char> {
        self.vim.macro_rec.as_ref().map(|(register, _)| *register)
    }

    /// Runs when insert mode ends: replays `3i…` and fans a block insert out
    /// to the other lines.
    pub(super) fn finish_insert(&mut self) {
        let Some(repeat) = self.vim.insert_repeat.take() else { return };
        for _ in 1..repeat.count {
            if let Some(below) = repeat.open {
                let line = self.buf.line_of(self.cursor);
                let (start, end) = (self.buf.line_start(line), self.buf.line_end(line));
                self.cursor = if below { self.edit(end..end, "\n").end } else { self.edit(start..start, "\n").start };
            }
            for input in &repeat.inputs {
                match input {
                    Input::Key(event) => self.insert_key(*event),
                    Input::Text(text) => self.type_text(text),
                }
            }
        }
        let Some(block) = repeat.block else { return };
        if self.suggesting || self.buf.line_of(self.cursor) != block.first || self.cursor <= block.start {
            return;
        }
        let text = self.buf.slice(block.start..self.cursor);
        for line in (block.first + 1..=block.last).rev() {
            if self.buf.line_len(line) >= block.col {
                let at = self.buf.line_start(line) + block.col;
                self.raw_edit(at..at, &text);
            }
        }
    }

    /// `(first line, last line, left col, right col)` of the block selection.
    pub fn block_rect(&self) -> (usize, usize, usize, usize) {
        let buf = &self.buf;
        let anchor = self.anchor.unwrap_or(self.cursor);
        let (a, c) = (buf.line_of(anchor), buf.line_of(self.cursor));
        let (ac, cc) = (anchor - buf.line_start(a), self.cursor - buf.line_start(c));
        (a.min(c), a.max(c), ac.min(cc), ac.max(cc))
    }

    /// Every selected range: one per line for a block, else the selection.
    pub fn selection_ranges(&self) -> Vec<Range<usize>> {
        if self.mode != Mode::VisualBlock {
            return self.selection().into_iter().collect();
        }
        let (first, last, left, right) = self.block_rect();
        (first..=last)
            .map(|line| {
                let (start, len) = (self.buf.line_start(line), self.buf.line_len(line));
                start + left.min(len)..start + (right + 1).min(len)
            })
            .collect()
    }

    fn block_op(&mut self, op: Op, register: Option<char>) {
        let (first, last, left, _) = self.block_rect();
        let ranges = self.selection_ranges();
        let text = ranges.iter().map(|range| self.buf.slice(range.clone())).collect::<Vec<_>>().join("\n");
        self.anchor = None;
        self.mode = Mode::Normal;
        let top = |ed: &Editor| ed.buf.line_start(first) + left.min(ed.buf.line_len(first));
        self.cursor = top(self);
        match op {
            Op::Yank => self.yank(register, text, false, true),
            Op::Delete | Op::Change => {
                self.yank(register, text, false, false);
                self.open_group();
                for range in ranges.into_iter().rev().filter(|range| !range.is_empty()) {
                    self.edit(range, "");
                }
                self.cursor = top(self);
                if op == Op::Change {
                    self.mode = Mode::Insert;
                    self.vim.insert_repeat = (!self.suggesting).then(|| InsertRepeat {
                        count: 1,
                        inputs: Vec::new(),
                        open: None,
                        block: Some(BlockInsert { first, last, col: left, start: self.cursor }),
                    });
                }
            }
            Op::Indent | Op::Dedent => {
                self.shift_lines(first, last, op == Op::Indent);
                if !self.suggesting {
                    self.cursor = motion::first_non_blank(&self.buf, first);
                }
            }
            Op::ToggleCase | Op::Lower | Op::Upper => {
                for range in ranges.into_iter().rev() {
                    let old = self.buf.slice(range.clone());
                    let new = match op {
                        Op::Lower => old.to_lowercase(),
                        Op::Upper => old.to_uppercase(),
                        _ => toggle_case(&old),
                    };
                    if new != old {
                        self.edit(range, &new);
                    }
                }
                if !self.suggesting {
                    self.cursor = top(self);
                }
            }
        }
    }

    fn repeat(&mut self, count: usize) {
        let inputs = self.vim.last_change.clone();
        if inputs.is_empty() {
            return;
        }
        self.vim.replaying = true;
        self.close_group();
        self.buf.begin(self.cursor);
        for _ in 0..count {
            for input in &inputs {
                match input {
                    Input::Key(event) => self.dispatch_key(*event),
                    Input::Text(text) => self.type_text(text),
                }
            }
        }
        self.close_group();
        self.buf.commit(self.cursor);
        self.vim.replaying = false;
        self.vim.suppress_record();
    }

    // ----- motions --------------------------------------------------------

    fn motion_target(&mut self, motion: Motion, count: Option<usize>, for_op: bool) -> Option<(usize, Kind)> {
        use Kind::*;
        let n = count.unwrap_or(1);
        let from = self.cursor;
        let line = self.buf.line_of(from);
        let last = self.buf.line_count() - 1;
        let repeat = |step: &dyn Fn(usize) -> usize| (0..n).fold(from, |pos, _| step(pos));
        let buf = &self.buf;
        Some(match motion {
            Motion::Left => {
                let start = buf.line_start(line);
                (repeat(&|pos| if pos > start { motion::prev_grapheme(buf, pos) } else { pos }), Exclusive)
            }
            Motion::Right => {
                let end = buf.line_end(line);
                (repeat(&|pos| if pos < end { motion::next_grapheme(buf, pos) } else { pos }), Exclusive)
            }
            Motion::Up | Motion::Down | Motion::DisplayUp | Motion::DisplayDown => {
                let down = matches!(motion, Motion::Down | Motion::DisplayDown);
                let display = matches!(motion, Motion::DisplayUp | Motion::DisplayDown)
                    || (count.is_none() && self.config.visual_line_motion);
                // Columns only line up on logical lines, and a macro cannot wait for layout.
                if display && !for_op && self.vim.playing == 0 && self.mode != Mode::VisualBlock {
                    let step = n as isize;
                    self.effects.push(Effect::VisualMove(if down { step } else { -step }));
                    return None;
                }
                let col = self.goal_col.unwrap_or(from - buf.line_start(line));
                let target = if down { (line + n).min(last) } else { line.saturating_sub(n) };
                self.goal_col = Some(col);
                self.keep_goal = true;
                (self.buf.line_start(target) + col.min(self.buf.line_len(target)), Line)
            }
            Motion::Page(fraction) => {
                if !for_op {
                    let rows = (self.view_rows.max(2) as f32 * fraction).round() as isize;
                    self.effects.push(Effect::VisualMove(rows * n as isize));
                }
                return None;
            }
            Motion::WordFwd(big) => (repeat(&|pos| motion::word_forward(buf, pos, big)), Exclusive),
            Motion::WordBack(big) => (repeat(&|pos| motion::word_backward(buf, pos, big)), Exclusive),
            Motion::WordEnd(big) => (repeat(&|pos| motion::word_end(buf, pos, big)), Inclusive),
            Motion::WordEndBack(big) => (repeat(&|pos| motion::word_end_backward(buf, pos, big)), Inclusive),
            Motion::LineStart => (buf.line_start(line), Exclusive),
            Motion::FirstNonBlank => (motion::first_non_blank(buf, line), Exclusive),
            Motion::LineEnd => (buf.line_end((line + n - 1).min(last)), Exclusive),
            Motion::Column => (buf.line_start(line) + (n - 1).min(buf.line_len(line)), Exclusive),
            Motion::FirstLine => (motion::first_non_blank(buf, count.map_or(0, |n| n - 1).min(last)), Line),
            Motion::LastLine => (motion::first_non_blank(buf, count.map_or(last, |n| n - 1).min(last)), Line),
            Motion::Find { ch, forward, till } => {
                self.vim.last_find = Some((ch, forward, till));
                let pos = motion::find_char(&self.buf, from, ch, forward, till, n)?;
                (pos, if forward { Inclusive } else { Exclusive })
            }
            Motion::RepeatFind { reverse } => {
                let (ch, forward, till) = self.vim.last_find?;
                let forward = forward != reverse;
                // Step off the previous `t` landing so `;` makes progress.
                let from = match (till, forward) {
                    (true, true) => (from + 1).min(buf.line_end(line)),
                    (true, false) => from.saturating_sub(1).max(buf.line_start(line)),
                    _ => from,
                };
                let pos = motion::find_char(buf, from, ch, forward, till, n)?;
                (pos, if forward { Inclusive } else { Exclusive })
            }
            Motion::ParaFwd => (repeat(&|pos| motion::paragraph_forward(buf, pos)), Exclusive),
            Motion::ParaBack => (repeat(&|pos| motion::paragraph_backward(buf, pos)), Exclusive),
            Motion::SentFwd => (repeat(&|pos| motion::sentence_forward(buf, pos)), Exclusive),
            Motion::SentBack => (repeat(&|pos| motion::sentence_backward(buf, pos)), Exclusive),
            Motion::MatchPair => (motion::match_pair(buf, from)?, Inclusive),
            Motion::SearchNext { reverse } => {
                let forward = self.vim.search.as_ref()?.forward != reverse;
                (self.search_target(from, forward, n)?, Exclusive)
            }
            Motion::StarSearch { forward } => {
                let word = motion::text_object(buf, from, false, 'w')?;
                let pattern = buf.slice(word.range.clone());
                if pattern.trim().is_empty() {
                    return None;
                }
                self.vim.search = Matcher::word(&pattern).map(|matcher| Search { matcher, forward });
                (self.search_target(word.range.start, forward, n)?, Exclusive)
            }
            Motion::Mark(mark) => ((*self.vim.marks.get(&mark)?).min(buf.len()), Exclusive),
            Motion::Suggestion { forward } => {
                self.refresh();
                let spans = self.doc.suggestions.iter().map(|suggestion| suggestion.span.start);
                let pos = if forward {
                    spans.clone().find(|&start| start > from).or(spans.clone().next())
                } else {
                    spans.clone().rfind(|&start| start < from).or(spans.clone().next_back())
                };
                if pos.is_none() {
                    self.info("no suggestions");
                }
                (pos?, Exclusive)
            }
        })
    }

    fn search_target(&mut self, from: usize, forward: bool, count: usize) -> Option<usize> {
        let matcher = &self.vim.search.as_ref()?.matcher;
        let pattern = matcher.pattern().to_string();
        let starts: Vec<usize> = matcher.find_all(&self.buf.text()).into_iter().map(|range| range.start).collect();
        self.vim.highlight = true;
        if starts.is_empty() {
            self.error(format!("Pattern not found: {pattern}"));
            return None;
        }
        let mut pos = from;
        for _ in 0..count.max(1) {
            pos = if forward {
                starts.iter().copied().find(|&start| start > pos).unwrap_or(starts[0])
            } else {
                starts.iter().copied().rev().find(|&start| start < pos).unwrap_or(starts[starts.len() - 1])
            };
        }
        Some(pos)
    }

    /// `n`/`N`, also bound to the find-next shortcut.
    pub(super) fn search_step(&mut self, same_direction: bool) {
        let Some(forward) = self.vim.search.as_ref().map(|search| search.forward == same_direction) else {
            return self.error("No previous search");
        };
        if let Some(pos) = self.search_target(self.cursor, forward, 1) {
            self.anchor = None;
            self.cursor = pos;
        }
    }

    pub(super) fn start_search(&mut self, pattern: &str, forward: bool) {
        self.vim.preview = None;
        if let Some(matcher) = Matcher::new(pattern, self.config.regex_search) {
            self.vim.search = Some(Search { matcher, forward });
        }
        self.search_step(true);
    }

    // ----- operators ------------------------------------------------------

    fn operate(&mut self, op: Op, target: Target, count: Option<usize>, register: Option<char>) {
        let origin = self.cursor;
        let buf = &self.buf;
        let last = buf.line_count() - 1;
        let range = match target {
            Target::Lines => {
                let line = buf.line_of(origin);
                OpRange::Lines(line, (line + count.unwrap_or(1) - 1).min(last))
            }
            Target::Object(around, ch) => match motion::text_object(buf, origin, around, ch) {
                Some(object) if object.linewise => OpRange::Lines(object.range.start, object.range.end),
                Some(object) => OpRange::Chars(object.range),
                None => return,
            },
            Target::Motion(Motion::WordFwd(big)) if op == Op::Change && buf.char_at(origin).is_some_and(|c| !c.is_whitespace()) => {
                // `cw` changes to the end of the word, leaving the space.
                let word = motion::text_object(buf, origin, false, if big { 'W' } else { 'w' });
                let mut end = word.map_or(origin, |word| word.range.end - 1);
                for _ in 1..count.unwrap_or(1) {
                    end = motion::word_end(buf, end, big);
                }
                OpRange::Chars(origin..end + 1)
            }
            Target::Motion(motion) => {
                let Some((to, kind)) = self.motion_target(motion, count, true) else { return };
                let buf = &self.buf;
                let (from, mut to) = (origin.min(to), origin.max(to));
                let (from_line, to_line) = (buf.line_of(from), buf.line_of(to));
                match kind {
                    Kind::Line => OpRange::Lines(from_line, to_line),
                    Kind::Inclusive => OpRange::Chars(from..motion::next_grapheme(buf, to)),
                    Kind::Exclusive => {
                        let crosses = to_line > from_line;
                        if matches!(motion, Motion::WordFwd(_)) {
                            // The last word moved over ends the range, not the next line's first word.
                            if crosses && to <= motion::first_non_blank(buf, to_line) {
                                to = buf.line_end(to_line - 1).max(from);
                            }
                            OpRange::Chars(from..to)
                        } else if crosses && to == buf.line_start(to_line) {
                            if from <= motion::first_non_blank(buf, from_line) {
                                OpRange::Lines(from_line, to_line - 1)
                            } else {
                                OpRange::Chars(from..buf.line_end(to_line - 1))
                            }
                        } else {
                            OpRange::Chars(from..to)
                        }
                    }
                }
            }
        };
        self.apply_op(op, range, register, origin);
    }

    fn lines_range(&self, first: usize, last: usize) -> Range<usize> {
        self.buf.line_start(first)..self.buf.line_end(last)
    }

    fn apply_op(&mut self, op: Op, range: OpRange, register: Option<char>, origin: usize) {
        let origin_line = self.buf.line_of(origin);
        let origin_col = origin - self.buf.line_start(origin_line);
        match (op, range) {
            (Op::Delete, OpRange::Chars(range)) => {
                if range.is_empty() {
                    return;
                }
                self.yank(register, self.buf.slice(range.clone()), false, false);
                let forward = range.start == origin;
                let pos = self.edit(range, "");
                self.cursor = if forward { pos.end } else { pos.start };
            }
            (Op::Delete, OpRange::Lines(first, last)) => {
                let text = format!("{}\n", self.buf.slice(self.lines_range(first, last)));
                self.yank(register, text, true, false);
                let range = if last + 1 < self.buf.line_count() {
                    self.buf.line_start(first)..self.buf.line_start(last + 1)
                } else if first > 0 {
                    self.buf.line_end(first - 1)..self.buf.len()
                } else {
                    0..self.buf.len()
                };
                let pos = self.edit(range, "");
                self.cursor = if self.suggesting {
                    pos.end
                } else {
                    motion::first_non_blank(&self.buf, first.min(self.buf.line_count() - 1))
                };
            }
            (Op::Change, range) => {
                let (range, linewise) = match range {
                    OpRange::Chars(range) => (range, false),
                    OpRange::Lines(first, last) => (self.lines_range(first, last), true),
                };
                let mut text = self.buf.slice(range.clone());
                if linewise {
                    text.push('\n');
                }
                self.open_group();
                if !range.is_empty() {
                    self.yank(register, text, linewise, false);
                }
                self.mode = Mode::Insert;
                self.cursor = if range.is_empty() { range.start } else { self.edit(range, "").end };
            }
            (Op::Yank, OpRange::Chars(range)) => {
                self.yank(register, self.buf.slice(range.clone()), false, true);
                self.cursor = range.start;
            }
            (Op::Yank, OpRange::Lines(first, last)) => {
                let text = format!("{}\n", self.buf.slice(self.lines_range(first, last)));
                self.yank(register, text, true, true);
                if first < origin_line {
                    self.cursor = self.buf.line_start(first) + origin_col.min(self.buf.line_len(first));
                }
            }
            (Op::Indent | Op::Dedent, range) => {
                let (first, last) = match range {
                    OpRange::Lines(first, last) => (first, last),
                    OpRange::Chars(range) => (
                        self.buf.line_of(range.start),
                        self.buf.line_of(range.end.saturating_sub(1).max(range.start)),
                    ),
                };
                self.shift_lines(first, last, op == Op::Indent);
                if !self.suggesting {
                    self.cursor = motion::first_non_blank(&self.buf, first);
                }
            }
            (Op::ToggleCase | Op::Lower | Op::Upper, range) => {
                let range = match range {
                    OpRange::Chars(range) => range,
                    OpRange::Lines(first, last) => self.lines_range(first, last),
                };
                let old = self.buf.slice(range.clone());
                let new = match op {
                    Op::Lower => old.to_lowercase(),
                    Op::Upper => old.to_uppercase(),
                    _ => toggle_case(&old),
                };
                let start = range.start;
                if new != old {
                    self.edit(range, &new);
                }
                if !self.suggesting {
                    self.cursor = start;
                }
            }
        }
    }

    fn yank(&mut self, register: Option<char>, text: String, linewise: bool, is_yank: bool) {
        let entry = Register { text, linewise };
        match register {
            Some('_') => return,
            Some('+' | '*') => self.clipboard.set(&entry.text),
            Some(name) if name.is_ascii_uppercase() => {
                let existing = self.vim.registers.entry(name.to_ascii_lowercase()).or_default();
                existing.text.push_str(&entry.text);
                existing.linewise |= linewise;
            }
            Some(name) => {
                self.vim.registers.insert(name, entry.clone());
            }
            None if self.config.system_clipboard => self.clipboard.set(&entry.text),
            None => {}
        }
        if is_yank {
            self.vim.registers.insert('0', entry.clone());
        }
        self.vim.registers.insert('"', entry);
    }

    fn register(&mut self, register: Option<char>) -> Option<Register> {
        let unnamed = self.vim.registers.get(&'"').cloned();
        // Text copied elsewhere is charwise; our own yank keeps its shape.
        let from_clipboard = |text: String, unnamed: Option<Register>| match unnamed {
            Some(ours) if ours.text == text => ours,
            _ => Register { text, linewise: false },
        };
        match register {
            Some('+' | '*') => self.clipboard.get().map(|text| from_clipboard(text, unnamed)),
            Some(name) => self.vim.registers.get(&name.to_ascii_lowercase()).cloned(),
            None if self.config.system_clipboard => match self.clipboard.get() {
                Some(text) => Some(from_clipboard(text, unnamed)),
                None => unnamed,
            },
            None => unnamed,
        }
    }

    fn put(&mut self, register: Option<char>, before: bool, count: usize) {
        let Some(entry) = self.register(register).filter(|entry| !entry.text.is_empty()) else {
            return self.error("Nothing in register");
        };
        let text = entry.text.repeat(count.max(1));
        if let Some(selection) = self.selection() {
            let replaced = Register {
                text: self.buf.slice(selection.clone()),
                linewise: self.mode == Mode::VisualLine,
            };
            let text = match (self.mode == Mode::VisualLine, entry.linewise) {
                (true, false) => format!("{text}\n"),
                (false, true) => format!("\n{text}"),
                _ => text,
            };
            self.anchor = None;
            self.mode = Mode::Normal;
            self.cursor = self.edit(selection, &text).start;
            self.vim.registers.insert('"', replaced);
            return;
        }
        let line = self.buf.line_of(self.cursor);
        if entry.linewise {
            let is_last = line + 1 >= self.buf.line_count();
            let pos = if before {
                let start = self.buf.line_start(line);
                self.edit(start..start, &text)
            } else if is_last {
                let end = self.buf.len();
                let mut pos = self.edit(end..end, &format!("\n{}", text.trim_end_matches('\n')));
                pos.start += 1;
                pos
            } else {
                let start = self.buf.line_start(line + 1);
                self.edit(start..start, &text)
            };
            self.cursor = motion::first_non_blank(&self.buf, self.buf.line_of(pos.start)).max(pos.start);
        } else {
            let end = self.buf.line_end(line);
            let at = if before || self.cursor >= end {
                self.cursor
            } else {
                motion::next_grapheme(&self.buf, self.cursor)
            };
            let pos = self.edit(at..at, &text);
            self.cursor = pos.end.saturating_sub(1).max(pos.start);
        }
    }

    fn join_lines(&mut self, line: usize, joins: usize, spaces: bool) {
        for _ in 0..joins {
            if line + 1 >= self.buf.line_count() {
                break;
            }
            let buf = &self.buf;
            let end = buf.line_end(line);
            let next = if spaces { motion::first_non_blank(buf, line + 1) } else { end + 1 };
            let bare = !spaces
                || next == buf.line_end(line + 1)
                || end == buf.line_start(line)
                || buf.char_at(end - 1).is_some_and(char::is_whitespace);
            let pos = self.edit(end..next, if bare { "" } else { " " });
            self.cursor = pos.start;
        }
    }
}

fn toggle_case(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            let swapped: Vec<char> = if c.is_uppercase() {
                c.to_lowercase().collect()
            } else {
                c.to_uppercase().collect()
            };
            swapped
        })
        .collect()
}
