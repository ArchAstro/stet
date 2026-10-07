use super::*;
use crate::input::parse_keys;

fn editor(text: &str) -> Editor {
    let config = Config {
        visual_line_motion: false,
        author: "Calvin".into(),
        ..Config::default()
    };
    let mut ed = Editor::new(config, Box::new(MemoryClipboard::default()));
    ed.primary = Primary::Super;
    let cursor = text.find('|').map_or(0, |at| text[..at].chars().count());
    ed.set_text(&text.replacen('|', "", 1));
    ed.cursor = cursor;
    ed
}

fn keys(ed: &mut Editor, notation: &str) {
    for event in parse_keys(notation) {
        ed.handle_key(event);
    }
}

fn show(ed: &Editor) -> String {
    let mut chars: Vec<char> = ed.buf.text().chars().collect();
    chars.insert(ed.cursor, '|');
    chars.into_iter().collect()
}

/// Runs `notation` on `text` (cursor at `|`) and returns text with the cursor.
fn run(text: &str, notation: &str) -> String {
    let mut ed = editor(text);
    keys(&mut ed, notation);
    show(&ed)
}

#[test]
fn basic_motions() {
    assert_eq!(run("|hello world", "w"), "hello |world");
    assert_eq!(run("hello |world", "b"), "|hello world");
    assert_eq!(run("|hello world", "e"), "hell|o world");
    assert_eq!(run("|hello world", "$"), "hello worl|d");
    assert_eq!(run("hello wor|ld", "0"), "|hello world");
    assert_eq!(run("  hello wor|ld", "^"), "  |hello world");
    assert_eq!(run("|a.b c", "w"), "a|.b c");
    assert_eq!(run("|a.b c", "W"), "a.b |c");
    assert_eq!(run("|one two three", "2w"), "one two |three");
    assert_eq!(run("one two th|ree", "ge"), "one tw|o three");
    assert_eq!(run("|ab", "lll"), "a|b");
    assert_eq!(run("a|b", "hhh"), "|ab");
    assert_eq!(run("a|b\ncd", "j"), "ab\nc|d");
    assert_eq!(run("abc|d\nx\nabcdef", "jj"), "abcd\nx\nabc|def");
    assert_eq!(run("one\n|two\nthree", "gg"), "|one\ntwo\nthree");
    assert_eq!(run("|one\ntwo\nthree", "G"), "one\ntwo\n|three");
    assert_eq!(run("|one\ntwo\nthree", "2G"), "one\n|two\nthree");
    assert_eq!(run("|one\n\ntwo", "w"), "one\n|\ntwo");
}

#[test]
fn find_and_pairs() {
    assert_eq!(run("|a,b,c", "f,"), "a|,b,c");
    assert_eq!(run("|a,b,c", "2f,"), "a,b|,c");
    assert_eq!(run("|a,b,c", "t,"), "|a,b,c");
    assert_eq!(run("|ab,c,d", "t,;"), "ab,|c,d");
    assert_eq!(run("a,b,|c", "F,"), "a,b|,c");
    assert_eq!(run("|a,b,c", "f,;"), "a,b|,c");
    assert_eq!(run("|a,b,c", "f,;,"), "a|,b,c");
    assert_eq!(run("|f(a[b]c)", "%"), "f(a[b]c|)");
    assert_eq!(run("f(a[b]c|)", "%"), "f|(a[b]c)");
}

#[test]
fn paragraph_and_sentence_motions() {
    assert_eq!(run("|a\nb\n\nc", "}"), "a\nb\n|\nc");
    assert_eq!(run("a\nb\n\n|c", "{"), "a\nb\n|\nc");
    assert_eq!(run("|One. Two! Three?", ")"), "One. |Two! Three?");
    assert_eq!(run("One. Two! |Three?", "("), "One. |Two! Three?");
}

#[test]
fn delete_operators() {
    assert_eq!(run("|hello world", "dw"), "|world");
    assert_eq!(run("hello |world", "dw"), "hello| ");
    assert_eq!(run("one |two\nthree", "dw"), "one| \nthree");
    assert_eq!(run("|one two three", "d2w"), "|three");
    assert_eq!(run("|one two three", "2dw"), "|three");
    assert_eq!(run("one |two three", "de"), "one | three");
    assert_eq!(run("one t|wo three", "db"), "one |wo three");
    assert_eq!(run("one |two three", "d$"), "one| ");
    assert_eq!(run("one |two three", "D"), "one| ");
    assert_eq!(run("one |two three", "d0"), "|two three");
    assert_eq!(run("a\n|b\nc", "dd"), "a\n|c");
    assert_eq!(run("a\nb\n|c", "dd"), "a\n|b");
    assert_eq!(run("|a", "dd"), "|");
    assert_eq!(run("|a\nb\nc\nd", "2dd"), "|c\nd");
    assert_eq!(run("|a\nb\nc\nd", "dj"), "|c\nd");
    assert_eq!(run("a\nb\n|c\nd", "dk"), "a\n|d");
    assert_eq!(run("a\n|b\nc", "dG"), "|a");
    assert_eq!(run("a\n|b\nc", "dgg"), "|c");
    assert_eq!(run("a|bcd", "x"), "a|cd");
    assert_eq!(run("a|bcd", "3x"), "|a");
    assert_eq!(run("abc|d", "X"), "ab|d");
    assert_eq!(run("f(|a, b)", "dt)"), "f(|)");
    assert_eq!(run("f(|a, b)", "df)"), "f|(");
    assert_eq!(run("|para one\nstill\n\nnext", "d}"), "|\nnext");
}

#[test]
fn text_objects() {
    assert_eq!(run("one t|wo three", "diw"), "one | three");
    assert_eq!(run("one t|wo three", "daw"), "one |three");
    assert_eq!(run("one two thr|ee", "daw"), "one tw|o");
    assert_eq!(run("say \"he|llo\" ok", "di\""), "say \"|\" ok");
    assert_eq!(run("say \"he|llo\" ok", "da\""), "say | ok");
    assert_eq!(run("f(a, (b|), c)", "di("), "f(a, (|), c)");
    assert_eq!(run("f(a, (b|), c)", "da("), "f(a, |, c)");
    assert_eq!(run("f(a, |(b), c)", "dib"), "f(a, (|), c)");
    assert_eq!(run("x [a|b] y", "ci[z<Esc>"), "x [|z] y");
    assert_eq!(run("a\n\nb|1\nb2\n\nc", "dip"), "a\n\n|\nc");
    assert_eq!(run("a\n\nb|1\nb2\n\nc", "dap"), "a\n\n|c");
    assert_eq!(run("One. Tw|o here. Three.", "dis"), "One. | Three.");
    assert_eq!(run("One. Tw|o here. Three.", "das"), "One. |Three.");
}

#[test]
fn change_and_insert() {
    assert_eq!(run("|hello world", "cwbye<Esc>"), "by|e world");
    assert_eq!(run("a |b c", "cwx<Esc>"), "a |x c");
    assert_eq!(run("one |two three", "c2wx<Esc>"), "one |x");
    assert_eq!(run("one |two", "Cx<Esc>"), "one |x");
    assert_eq!(run("a\n|bcd\ne", "ccx<Esc>"), "a\n|x\ne");
    assert_eq!(run("a|bc", "sx<Esc>"), "a|xc");
    assert_eq!(run("ab|c", "Sx<Esc>"), "|x");
    assert_eq!(run("a|b", "ix<Esc>"), "a|xb");
    assert_eq!(run("a|b", "ax<Esc>"), "ab|x");
    assert_eq!(run("  a|b", "Ix<Esc>"), "  |xab");
    assert_eq!(run("|ab", "Ax<Esc>"), "ab|x");
    assert_eq!(run("|ab\ncd", "ox<Esc>"), "ab\n|x\ncd");
    assert_eq!(run("ab\n|cd", "Ox<Esc>"), "ab\n|x\ncd");
    assert_eq!(run("|", "ihi<Esc>"), "h|i");
    assert_eq!(run("a|bc", "rx"), "a|xc");
    assert_eq!(run("a|bcd", "2rx"), "ax|xd");
    assert_eq!(run("a|Bc", "~~"), "ab|C");
    assert_eq!(run("|a\nb\nc", "J"), "a| b\nc");
    assert_eq!(run("|a\n  b\nc", "3J"), "a b| c");
    assert_eq!(run("|a\nb", "gJ"), "a|b");
}

#[test]
fn case_and_indent_operators() {
    assert_eq!(run("|hello world", "gUiw"), "|HELLO world");
    assert_eq!(run("HEL|LO world", "guiw"), "|hello world");
    assert_eq!(run("|Hello", "g~~"), "|hELLO");
    assert_eq!(run("|a\nb", ">>"), "    |a\nb");
    assert_eq!(run("    |a\nb", "<<"), "|a\nb");
    assert_eq!(run("|a\nb\nc", ">j"), "    |a\n    b\nc");
    assert_eq!(run("|a\nb\nc", "Vj>"), "    |a\n    b\nc");
}

#[test]
fn yank_put_and_registers() {
    assert_eq!(run("|one two", "yiwwP"), "one on|etwo");
    assert_eq!(run("|one two", "yiwwp"), "one ton|ewo");
    assert_eq!(run("|a\nb", "yyp"), "a\n|a\nb");
    assert_eq!(run("a\n|b", "yyP"), "a\n|b\nb");
    assert_eq!(run("a\n|b", "yyp"), "a\nb\n|b");
    assert_eq!(run("|a\nb", "ddp"), "b\n|a");
    assert_eq!(run("|ab", "xp"), "b|a");
    assert_eq!(run("|a\nb", "yy2p"), "a\n|a\na\nb");
    assert_eq!(run("|one two", "\"ayiww\"byiw\"aP"), "one on|etwo");
    assert_eq!(run("|one two", "yiwwdiw\"0P"), "oneon|e ");
    assert_eq!(run("|one two", "\"_dwP"), "|two");
    assert_eq!(run("|one two", "yiwwviwp"), "one |one");

    let mut ed = editor("|word");
    keys(&mut ed, "yiw");
    assert_eq!(ed.clipboard.get().as_deref(), Some("word"));
    ed.clipboard.set("ext");
    keys(&mut ed, "P");
    assert_eq!(show(&ed), "ex|tword");
}

#[test]
fn visual_mode() {
    assert_eq!(run("|one two three", "vwd"), "|wo three");
    assert_eq!(run("|one two three", "ved"), "| two three");
    assert_eq!(run("a\n|b\nc\nd", "Vjd"), "a\n|d");
    assert_eq!(run("one |two three", "viwc2<Esc>"), "one |2 three");
    assert_eq!(run("one |two three", "viwU"), "one |TWO three");
    assert_eq!(run("one two| three", "vbo<Esc>"), "one two| three");
    assert_eq!(run("|a\nb\nc", "VjJ"), "a| b\nc");
    assert_eq!(run("a (b |c) d", "vi(d"), "a (|) d");
    assert_eq!(run("a\n\nb|1\nb2\n\nc", "vipd"), "a\n\n|\nc");
    let mut ed = editor("|one two");
    keys(&mut ed, "ve");
    assert_eq!(ed.selection(), Some(0..3));
    keys(&mut ed, "V");
    assert_eq!(ed.mode, Mode::VisualLine);
    assert_eq!(ed.selection(), Some(0..7));
    keys(&mut ed, "<Esc>");
    assert_eq!((ed.mode, ed.selection()), (Mode::Normal, None));
}

#[test]
fn undo_redo_and_dot() {
    assert_eq!(run("|one two three", "dwu"), "|one two three");
    assert_eq!(run("|one two three", "dwdwuu<C-r>"), "|two three");
    assert_eq!(run("|one", "ihello <Esc>u"), "|one");
    assert_eq!(run("|one two three", "dw."), "|three");
    assert_eq!(run("|one two three", "cwx<Esc>w."), "x |x three");
    assert_eq!(run("|a\nb\nc", "A!<Esc>j.j."), "a!\nb!\nc|!");
    assert_eq!(run("|a\nb\nc\nd", "dd.."), "|d");
    assert_eq!(run("|ab", "x.u"), "|b");
    assert_eq!(run("|a b c d", "dw2."), "|d");
    assert_eq!(run("|a b c d", "dw2.u"), "|b c d");
    assert_eq!(run("|one", "ia<Esc>ub"), "|one");
    let mut ed = editor("|x");
    keys(&mut ed, "u");
    assert_eq!(ed.message.as_ref().unwrap().text, "Already at oldest change");
    assert!(!ed.buf.is_dirty());
    keys(&mut ed, "iy<Esc>");
    assert!(ed.buf.is_dirty());
    keys(&mut ed, "u");
    assert!(!ed.buf.is_dirty());
}

#[test]
fn insert_mode_keys() {
    assert_eq!(run("|", "iab<BS>c<Esc>"), "a|c");
    assert_eq!(run("|", "ione two<C-w>x<Esc>"), "one |x");
    assert_eq!(run("|", "ione two<C-u>x<Esc>"), "|x");
    assert_eq!(run("a|b", "i<Del><Esc>"), "|a");
    assert_eq!(run("|ab", "i<Right><Right>x<Left><Left>y<Esc>"), "a|ybx");
    assert_eq!(run("ab|c", "i<Home>x<End>y<Esc>"), "xabc|y");
    assert_eq!(run("|", "i👨‍👩‍👧x<BS><BS><Esc>"), "|");
    assert_eq!(run("|é", "i<Right><BS><Esc>"), "|");
    assert_eq!(run("|one two", "i<M-Right><M-Right>!<Esc>"), "one two|!");
    assert_eq!(run("|ab", "i<S-Right><S-Right>x<Esc>"), "|x");
    assert_eq!(run("|", "i<Tab>x<Esc>"), "    |x");
}

#[test]
fn markdown_list_continuation() {
    assert_eq!(run("- one|", "a<CR>two<Esc>"), "- one\n- tw|o");
    assert_eq!(run("1. one|", "a<CR>two<Esc>"), "1. one\n2. tw|o");
    assert_eq!(run("- [x] done|", "a<CR>next<Esc>"), "- [x] done\n- [ ] nex|t");
    assert_eq!(run("  * a|", "a<CR>b<Esc>"), "  * a\n  * |b");
    assert_eq!(run("> quote|", "a<CR>more<Esc>"), "> quote\n> mor|e");
    assert_eq!(run("- one\n- |", "a<CR>x<Esc>"), "- one\n|x");
    assert_eq!(run("  code|", "a<CR>x<Esc>"), "  code\n  |x");
    assert_eq!(run("- a|", "a<Tab><Esc>"), "    - |a");
    assert_eq!(run("    - a|", "a<S-Tab><Esc>"), "- |a");
    assert_eq!(run("- [ ] |todo", "gt"), "- [x] |todo");
    assert_eq!(run("- [x] |todo", "gt"), "- [ ] |todo");
}

#[test]
fn search_and_substitute() {
    assert_eq!(run("|foo bar foo baz", "/foo<CR>"), "foo bar |foo baz");
    assert_eq!(run("|foo bar foo baz", "/foo<CR>n"), "|foo bar foo baz");
    assert_eq!(run("foo bar |foo baz", "?bar<CR>"), "foo |bar foo baz");
    assert_eq!(run("|Foo foo FOO", "/foo<CR>n"), "Foo foo |FOO");
    assert_eq!(run("|Foo foo FOO", "/FOO<CR>"), "Foo foo |FOO");
    assert_eq!(run("|foo bar foo", "*"), "foo bar |foo");
    assert_eq!(run("foo bar |foo", "#"), "|foo bar foo");
    assert_eq!(run("|foo bar foo baz", "/foo<CR>N"), "|foo bar foo baz");
    assert_eq!(run("|a b a\na", ":s/a/x/<CR>"), "|x b a\na");
    assert_eq!(run("|a b a\na", ":s/a/x/g<CR>"), "|x b x\na");
    assert_eq!(run("a b a\n|a", ":%s/a/xy/g<CR>"), "|xy b xy\nxy");
    assert_eq!(run("|foo bar foo", "/foo<CR>dn"), "|foo");
    let mut ed = editor("|abc");
    keys(&mut ed, "/zzz<CR>");
    assert!(ed.message.as_ref().unwrap().error);
    keys(&mut ed, "/b<CR>");
    assert_eq!(ed.search_highlight().map(Matcher::pattern), Some("b"));
    keys(&mut ed, ":noh<CR>");
    assert!(ed.search_highlight().is_none());
}

#[test]
fn command_line() {
    assert_eq!(run("a\nb\n|c", ":1<CR>"), "|a\nb\nc");
    assert_eq!(run("|a\nb\nc", ":$<CR>"), "a\nb\n|c");
    let mut ed = editor("|x");
    keys(&mut ed, ":theme nord<CR>");
    assert_eq!(ed.theme.name, "nord");
    assert_eq!(ed.take_effects(), vec![Effect::ThemeChanged]);
    keys(&mut ed, ":theme no<Tab><CR>");
    assert_eq!(ed.theme.name, "nord");
    keys(&mut ed, ":theme nord<Tab><CR>");
    assert_eq!(ed.theme.name, "latte");
    keys(&mut ed, ":theme bogus<CR>");
    assert!(ed.message.as_ref().unwrap().error);
    ed.take_effects();
    keys(&mut ed, ":q<CR>");
    assert_eq!(ed.take_effects(), vec![Effect::Quit]);
    keys(&mut ed, "ix<Esc>:q<CR>");
    assert!(ed.take_effects().is_empty() && ed.message.as_ref().unwrap().error);
    keys(&mut ed, ":q!<CR>");
    assert_eq!(ed.take_effects(), vec![Effect::Quit]);
    keys(&mut ed, ":nonsense<CR>");
    assert_eq!(ed.message.as_ref().unwrap().text, "Not an editor command: nonsense");
    keys(&mut ed, ":abc<Esc>");
    assert!(ed.cmdline.is_none());
    keys(&mut ed, ":<BS>");
    assert!(ed.cmdline.is_none());
}

#[test]
fn display_line_motion_is_delegated_to_the_shell() {
    let mut ed = editor("|a\nb\nc");
    ed.config.visual_line_motion = true;
    keys(&mut ed, "j");
    assert_eq!(ed.take_effects(), vec![Effect::VisualMove(1)]);
    keys(&mut ed, "2j");
    assert_eq!(show(&ed), "a\nb\n|c");
    keys(&mut ed, "gk<C-d>");
    ed.view_rows = 20;
    assert_eq!(ed.take_effects(), vec![Effect::VisualMove(-1), Effect::VisualMove(15)]);
    keys(&mut ed, "zz");
    assert_eq!(ed.take_effects(), vec![Effect::Scroll(ScrollTo::Center)]);
    keys(&mut ed, "ggdj");
    assert_eq!(show(&ed), "|c");
}

#[test]
fn standard_mode_behaves_like_a_text_field() {
    let mut ed = editor("|");
    keys(&mut ed, ":novim<CR>");
    assert_eq!(ed.mode, Mode::Insert);
    keys(&mut ed, "hello world<Esc>jk");
    assert_eq!(show(&ed), "hello worldjk|");
    keys(&mut ed, "<D-z>");
    assert_eq!(show(&ed), "hello |", "undo steps back a word");
    keys(&mut ed, "<D-z><D-z>");
    assert_eq!(show(&ed), "|");
    keys(&mut ed, "<D-S-z>");
    assert_eq!(show(&ed), "hello |");
    keys(&mut ed, "<D-a>");
    assert_eq!(ed.selection(), Some(0..6));
    keys(&mut ed, "<D-c><Right><D-v>");
    assert_eq!(show(&ed), "hello hello |");
    keys(&mut ed, "<S-Left><S-Left><D-x>");
    assert_eq!(show(&ed), "hello hell|");
    keys(&mut ed, "<S-Home><D-b>");
    assert_eq!(show(&ed), "**hello hell**|");
    keys(&mut ed, "<D-i>");
    assert_eq!(show(&ed), "**hello hell***|*");
}

#[test]
fn shortcuts_and_wrapping_in_vim_mode() {
    assert_eq!(run("one |two", "viw<D-b><Esc>"), "one **two*|*");
    assert_eq!(run("one |two", "viw<D-k>"), "one [two](|)");
    let mut ed = editor("|x");
    keys(&mut ed, "<D-=><D-0><D-S-t><Down><CR>");
    assert_eq!(
        ed.take_effects(),
        vec![
            Effect::Zoom(1),
            Effect::Zoom(0),
            Effect::ThemeChanged,
            Effect::ThemeChanged
        ]
    );
    assert_eq!(ed.theme.name, "ristretto");
    ed.primary = Primary::Ctrl;
    keys(&mut ed, "<C-r>");
    assert!(ed.take_effects().is_empty(), "Ctrl-r stays redo under vim");
    keys(&mut ed, "<C-s>");
    assert_eq!(ed.take_effects(), vec![Effect::SaveAsDialog]);
}

#[test]
fn mouse_selection() {
    let mut ed = editor("|one two three");
    ed.mouse_down(5, 1, false);
    assert_eq!(show(&ed), "one t|wo three");
    ed.mouse_drag(9);
    ed.mouse_up();
    assert_eq!((ed.mode, ed.selection()), (Mode::Visual, Some(5..10)));
    ed.mouse_down(5, 2, false);
    assert_eq!(ed.selection(), Some(4..7));
    ed.mouse_down(5, 3, false);
    assert_eq!((ed.mode, ed.selection()), (Mode::VisualLine, Some(0..13)));
    ed.mouse_down(0, 1, false);
    assert_eq!((ed.mode, ed.selection()), (Mode::Normal, None));
    ed.mouse_down(3, 1, true);
    assert_eq!(ed.selection(), Some(0..4));
}

// ----- suggestion mode ------------------------------------------------------

fn suggesting(text: &str) -> Editor {
    let mut ed = editor(text);
    ed.suggesting = true;
    ed
}

/// Replaces generated ids so expectations stay readable.
fn normalized(ed: &mut Editor) -> String {
    ed.refresh();
    let mut text = ed.buf.text();
    for (index, suggestion) in ed.doc().suggestions.iter().enumerate() {
        text = text.replace(&suggestion.id, &format!("s_{index:08}"));
    }
    text
}

#[test]
fn typing_records_one_growing_insertion() {
    let mut ed = suggesting("Hell|o world");
    keys(&mut ed, "a there<Esc>");
    assert_eq!(
        normalized(&mut ed),
        "Hello{++ there++}{>>id:s_00000000 by:Calvin<<} world"
    );
    assert_eq!(ed.doc().suggestions.len(), 1);
    let id = ed.doc().suggestions[0].id.clone();
    assert!(id.starts_with("s_") && id.len() == 10);
    keys(&mut ed, "a!<BS><BS><Esc>");
    assert_eq!(
        normalized(&mut ed),
        "Hello{++ ther++}{>>id:s_00000000 by:Calvin<<} world"
    );
    assert_eq!(ed.doc().suggestions[0].id, id, "extending keeps the id");
}

#[test]
fn retracting_an_insertion_removes_the_markup() {
    let mut ed = suggesting("ab|");
    keys(&mut ed, "axy<BS><BS><Esc>");
    assert_eq!(ed.buf.text(), "ab");
}

#[test]
fn deletions_merge_while_backspacing_or_deleting_forward() {
    let mut ed = suggesting("one tw|o three");
    keys(&mut ed, "a<BS><BS><BS><Esc>");
    assert_eq!(normalized(&mut ed), "one {--two--}{>>id:s_00000000 by:Calvin<<} three");
    let mut ed = suggesting("one |two three");
    keys(&mut ed, "xxx");
    assert_eq!(normalized(&mut ed), "one {--two--}{>>id:s_00000000 by:Calvin<<} three");
    let mut ed = suggesting("one |two three");
    keys(&mut ed, "dw");
    assert_eq!(normalized(&mut ed), "one {--two --}{>>id:s_00000000 by:Calvin<<}three");
}

#[test]
fn change_becomes_a_replacement() {
    let mut ed = suggesting("one |two three");
    keys(&mut ed, "cw2<Esc>");
    assert_eq!(
        normalized(&mut ed),
        "one {~~two~>2~~}{>>id:s_00000000 by:Calvin<<} three"
    );
    keys(&mut ed, "atwo<Esc>");
    assert_eq!(
        normalized(&mut ed),
        "one {~~two~>2two~~}{>>id:s_00000000 by:Calvin<<} three"
    );
    let mut ed = suggesting("one |two three");
    keys(&mut ed, "viwcTWO<Esc>");
    assert_eq!(
        normalized(&mut ed),
        "one {~~two~>TWO~~}{>>id:s_00000000 by:Calvin<<} three"
    );
}

#[test]
fn accept_and_reject_resolve_with_plain_edits() {
    let source = "one {~~two~>2~~}{>>id:s_0123abcd by:Agent<<} three {++four ++}{>>id:s_0123abce by:Agent<<}";
    assert_eq!(
        run(&format!("|{source}"), "wgsa"),
        "one |2 three {++four ++}{>>id:s_0123abce by:Agent<<}"
    );
    assert_eq!(
        run(&format!("|{source}"), "wgsr"),
        "one |two three {++four ++}{>>id:s_0123abce by:Agent<<}"
    );
    assert_eq!(
        run(&format!("|{source}"), "gsa"),
        "one |2 three {++four ++}{>>id:s_0123abce by:Agent<<}"
    );
    assert_eq!(run(&format!("|{source}"), ":acceptall<CR>"), "|one 2 three four ");
    assert_eq!(run(&format!("|{source}"), ":rejectall<CR>"), "|one two three ");
    assert_eq!(run(&format!("|{source}"), ":rejectall<CR>u"), format!("|{source}"));
    assert_eq!(
        run(&format!("|{source}"), "]s]s"),
        "one {~~two~>2~~}{>>id:s_0123abcd by:Agent<<} three |{++four ++}{>>id:s_0123abce by:Agent<<}"
    );
    assert_eq!(
        run(&format!("|{source}"), "[s"),
        "one {~~two~>2~~}{>>id:s_0123abcd by:Agent<<} three |{++four ++}{>>id:s_0123abce by:Agent<<}"
    );
    let mut ed = editor("|plain");
    keys(&mut ed, "gsa");
    assert_eq!(ed.message.as_ref().unwrap().text, "no suggestion here");
    let mut ed = suggesting(&format!("|{source}"));
    keys(&mut ed, "wgsa");
    assert_eq!(
        show(&ed),
        "one |2 three {++four ++}{>>id:s_0123abce by:Agent<<}",
        "resolving is never itself a suggestion"
    );
}

#[test]
fn suggestion_mode_refuses_what_it_cannot_represent() {
    let source = "a {++b++}{>>id:s_0123abcd by:Agent<<} c";
    let mut ed = suggesting("a {++|b++}{>>id:s_0123abcd by:Agent<<} c");
    keys(&mut ed, "ix");
    assert_eq!(ed.buf.text(), source, "another author's suggestion is not editable");
    assert!(ed.message.as_ref().unwrap().error);

    let mut ed = suggesting("|```\ncode\n```\n");
    keys(&mut ed, "jx");
    assert_eq!(ed.buf.text(), "```\ncode\n```\n");
    assert!(ed.message.as_ref().unwrap().text.contains("code fence"));

    // A trailing `{` would read as a nested opener, so it is refused.
    let mut ed = suggesting("ab|");
    keys(&mut ed, "a{");
    assert!(ed.message.as_ref().unwrap().error);
    keys(&mut ed, "++<Esc>");
    assert_eq!(normalized(&mut ed), "ab{++++++}{>>id:s_00000000 by:Calvin<<}");
    assert_eq!(ed.doc().suggestions[0].new_text, "++");

    let mut ed = suggesting("a {b|");
    keys(&mut ed, "hhvld");
    assert_eq!(normalized(&mut ed), "a {--{b--}{>>id:s_00000000 by:Calvin<<}");
}

#[test]
fn suggestions_undo_as_ordinary_edits() {
    let mut ed = suggesting("one |two");
    keys(&mut ed, "dwu");
    assert_eq!(show(&ed), "one |two");
    keys(&mut ed, "A more<Esc>u");
    assert_eq!(ed.buf.text(), "one two");
}

#[test]
fn suggestion_toggle_and_author() {
    let mut ed = editor("|x");
    keys(&mut ed, ":suggest<CR>");
    assert!(ed.suggesting);
    keys(&mut ed, ":author Ada L.<CR>ay<Esc>");
    assert_eq!(normalized(&mut ed), "x{++y++}{>>id:s_00000000 by:Ada L.<<}");
    keys(&mut ed, ":author bad<<}<CR>");
    assert_eq!(ed.config.author, "Ada L.");
    keys(&mut ed, "<D-S-e>");
    assert!(!ed.suggesting);
}

// ----- files ----------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("md-core-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn save_is_atomic_and_tracks_dirty_state() {
    let dir = temp_dir("save");
    let path = dir.join("note.md");
    std::fs::write(&path, "one\r\ntwo\r\n").unwrap();
    let mut ed = editor("");
    ed.open(&path).unwrap();
    assert_eq!(ed.buf.text(), "one\ntwo\n");
    keys(&mut ed, "A!<Esc>");
    assert!(ed.buf.is_dirty());
    keys(&mut ed, ":w<CR>");
    assert!(!ed.buf.is_dirty(), "{:?}", ed.message);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "one!\r\ntwo\r\n",
        "line endings survive"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "no temp files remain: {leftovers:?}");
    keys(&mut ed, "u");
    assert!(ed.buf.is_dirty(), "undo past the save point is a change");
    keys(&mut ed, ":wq<CR>");
    assert_eq!(ed.take_effects(), vec![Effect::Quit]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\r\ntwo\r\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn external_changes_are_never_clobbered_silently() {
    let dir = temp_dir("conflict");
    let path = dir.join("note.md");
    std::fs::write(&path, "mine\n").unwrap();
    let mut ed = editor("");
    ed.open(&path).unwrap();
    // A clean buffer follows the disk.
    std::fs::write(&path, "theirs\n").unwrap();
    filetime_bump(&path, 5);
    ed.check_disk();
    assert_eq!(ed.buf.text(), "theirs\n");
    // A dirty buffer refuses a plain write.
    keys(&mut ed, "Ax<Esc>");
    std::fs::write(&path, "theirs again\n").unwrap();
    filetime_bump(&path, 10);
    ed.check_disk();
    assert!(ed.message.as_ref().unwrap().error);
    keys(&mut ed, ":w<CR>");
    assert!(ed.message.as_ref().unwrap().error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs again\n");
    keys(&mut ed, ":w!<CR>");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirsx\n");
    std::fs::remove_dir_all(dir).unwrap();
}

fn filetime_bump(path: &Path, seconds: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() + std::time::Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn opening_rejects_binary_and_starts_new_files() {
    let dir = temp_dir("open");
    let binary = dir.join("blob.md");
    std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
    let mut ed = editor("|keep");
    assert!(ed.open(&binary).is_err());
    assert_eq!(ed.buf.text(), "keep", "a failed open leaves the buffer alone");
    let fresh = dir.join("new.md");
    ed.open(&fresh).unwrap();
    assert_eq!(ed.buf.text(), "");
    keys(&mut ed, "ihi<Esc>:w<CR>");
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "hi\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn focus_lines_cover_the_current_paragraph() {
    let ed = editor("a\n\nb1\n|b2\nb3\n\nc");
    assert_eq!(ed.focus_lines(), 2..5);
    let ed = editor("a\n|\nb");
    assert_eq!(ed.focus_lines(), 1..2);
}

#[test]
fn random_key_storms_never_panic_or_break_invariants() {
    let alphabet: Vec<&str> = vec![
        "h", "j", "k", "l", "w", "b", "e", "0", "$", "G", "g", "d", "c", "y", "p", "P", "x", "u", "<C-r>", ".", "v",
        "V", "i", "a", "o", "O", "A", "<Esc>", "<Esc>", "<CR>", "<BS>", "<Del>", "<Tab>", "J", "~", "r", "f", "t", ";",
        "}", "{", "(", ")", "%", "n", "*", "/", ":", "s", "é", "😀", "-", " ", "[", "]", "\"", ">", "<lt>", "2", "3",
        "{", "+", "~", "<Left>", "<Right>", "<Up>", "<Down>", "<S-Left>", "<D-z>", "<D-b>", "<C-w>", "gsa", "`",
    ];
    let seed_text = "# Title\n\nSome *text* with {++a suggestion++}{>>id:s_0123abcd by:Calvin<<} here.\n\n- [ ] item\n- two\n\n```\ncode\n```\n\nEnd 😀.\n";
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for round in 0..40 {
        let mut ed = editor(seed_text);
        ed.suggesting = round % 3 == 0;
        ed.config.vim = round % 5 != 4;
        if !ed.config.vim {
            ed.mode = Mode::Insert;
        }
        for _ in 0..400 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            keys(&mut ed, alphabet[(state % alphabet.len() as u64) as usize]);
            assert!(ed.cursor <= ed.buf.len());
            if let Some(selection) = ed.selection() {
                assert!(selection.end <= ed.buf.len());
            }
            ed.refresh();
            assert_eq!(ed.doc().line_count(), ed.buf.line_count());
            ed.take_effects();
        }
    }
}

#[test]
fn counted_inserts_repeat_on_escape() {
    assert_eq!(run("|", "3ix<Esc>"), "xx|x");
    assert_eq!(run("|a", "2ohi<Esc>"), "a\nhi\nh|i");
    assert_eq!(run("a|b", "3A!<Esc>"), "ab!!|!");
    assert_eq!(run("|", "2iab<Esc>."), "abaaba|bb");
}

#[test]
fn macros_record_and_replay() {
    assert_eq!(run("|one\ntwo\nthree", "qaA!<Esc>jq@a@@"), "one!\ntwo!\nthree|!");
    assert_eq!(run("|a\nb\nc\nd", "qqx0jq2@q"), "\n\n\n|d");
    let mut ed = editor("|x");
    keys(&mut ed, "qa");
    assert_eq!(ed.recording_macro(), Some('a'));
    keys(&mut ed, "q@b");
    assert_eq!(ed.recording_macro(), None);
    assert!(ed.message.as_ref().unwrap().error);
}

#[test]
fn block_visual_edits_columns() {
    assert_eq!(run("|abc\nabc\nabc", "<C-v>jlx"), "|c\nc\nabc");
    assert_eq!(run("|abc\nabc\nabc", "<C-v>jjI- <Esc>"), "-| abc\n- abc\n- abc");
    assert_eq!(run("|abc\nabc", "<C-v>jlA!<Esc>"), "ab|!c\nab!c");
    assert_eq!(run("a|bc\nabc", "<C-v>jcXY<Esc>"), "aX|Yc\naXYc");
    assert_eq!(run("|abc\nabc", "<C-v>jlU"), "|ABc\nABc");
    assert_eq!(run("|abc\nabc", "<C-v>jr."), "|.bc\n.bc");
    assert_eq!(run("|abc\na\nabc", "l<C-v>jjld"), "|a\na\na");
    let mut ed = editor("|abc\nabc");
    keys(&mut ed, "l<C-v>jl");
    assert_eq!(ed.mode, Mode::VisualBlock);
    assert_eq!(ed.selection_ranges(), vec![1..3, 5..7]);
    keys(&mut ed, "y");
    keys(&mut ed, "P");
    assert_eq!(ed.buf.text(), "abc\nbcbc\nabc");
}

#[test]
fn search_and_substitute_use_regexes() {
    assert_eq!(run("|foo bar baz", "/b.z<CR>"), "foo bar |baz");
    assert_eq!(run("|a1 b22 c333", "/\\d{2,}<CR>n"), "a1 b22 c|333");
    assert_eq!(run("|x (y", "/(y<CR>"), "x |(y");
    assert_eq!(
        run("|ann@home bob@work", ":s/(\\w+)@(\\w+)/\\2:\\1/g<CR>"),
        "|home:ann work:bob"
    );
    assert_eq!(run("|aXa", ":s/x/[&]/<CR>"), "a|[X]a");
    assert_eq!(run("|foo foobar foo", "*"), "foo foobar |foo");
    let mut ed = editor("|abc abd");
    keys(&mut ed, "/ab");
    assert_eq!(
        ed.search_highlight().map(|m| m.find_all("abc abd")),
        Some(vec![0..2, 4..6])
    );
    keys(&mut ed, "<Esc>");
    assert!(ed.search_highlight().is_none());
}

#[test]
fn tabs_keep_each_documents_state() {
    let mut ed = editor("|first");
    keys(&mut ed, "x");
    ed.new_tab();
    assert_eq!((ed.tab_count(), ed.active, ed.buf.text().as_str()), (2, 1, ""));
    keys(&mut ed, "isecond<Esc>");
    ed.scroll_line = 7;
    ed.switch_tab(0);
    assert_eq!((show(&ed).as_str(), ed.scroll_line), ("|irst", 0));
    keys(&mut ed, "u");
    assert_eq!(ed.buf.text(), "first");
    keys(&mut ed, "]t");
    assert_eq!((ed.buf.text().as_str(), ed.scroll_line), ("second", 7));
    let titles: Vec<(bool, bool)> = ed.tabs().iter().map(|tab| (tab.dirty, tab.active)).collect();
    assert_eq!(titles, vec![(false, false), (true, true)]);
    assert_eq!(ed.dirty_tabs(), vec![1]);
    keys(&mut ed, ":q<CR>");
    assert_eq!(ed.tab_count(), 2, "unsaved changes block :q");
    keys(&mut ed, ":q!<CR>");
    assert_eq!((ed.tab_count(), ed.buf.text().as_str()), (1, "first"));
    assert!(ed.take_effects().is_empty());
    keys(&mut ed, ":q<CR>");
    assert_eq!(ed.take_effects(), vec![Effect::Quit]);
}

#[test]
fn links_are_found_under_the_cursor() {
    let wiki = |note: &str, anchor: Option<&str>| {
        Some(Link::Wiki {
            note: note.into(),
            anchor: anchor.map(str::to_string),
        })
    };
    assert_eq!(link_in("see [[Other Note]] now", 6), wiki("Other Note", None));
    assert_eq!(link_in("see [[Other Note]] now", 3), None);
    assert_eq!(link_in("[[a/b|shown]]", 4), wiki("a/b", None));
    assert_eq!(link_in("[[Note#Some Heading]]", 4), wiki("Note", Some("Some Heading")));
    assert_eq!(
        link_in("go [here](sub/my%20file.md#top \"t\") x", 5),
        Some(Link::File {
            path: "sub/my file.md".into(),
            anchor: Some("top".into())
        })
    );
    assert_eq!(
        link_in("[x](https://example.com/a)", 2),
        Some(Link::Url("https://example.com/a".into()))
    );
    assert_eq!(
        link_in("bare https://example.com/a, ok", 10),
        Some(Link::Url("https://example.com/a".into()))
    );
    assert_eq!(
        link_in("[x](#local)", 1),
        Some(Link::File {
            path: String::new(),
            anchor: Some("local".into())
        })
    );
}

#[test]
fn notes_link_into_a_knowledge_base() {
    let dir = temp_dir("links");
    std::fs::create_dir_all(dir.join("deep/er")).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        "# Index\n\nSee [[Plan]] and [[Ideas#Later]] and [guide](deep/guide.md).\nNew: [[Fresh Note]]\n",
    )
    .unwrap();
    std::fs::write(dir.join("deep/er/Plan.md"), "# Plan\n\nBack to [[index]].\n").unwrap();
    std::fs::write(
        dir.join("Ideas.md"),
        "# Ideas\n\n## Now\n\n## Later\n\ntext, see [the index](index.md)\n",
    )
    .unwrap();
    std::fs::write(dir.join("deep/guide.md"), "guide, no links\n").unwrap();

    let mut ed = editor("");
    ed.open(&dir.join("index.md")).unwrap();
    assert_eq!(ed.workspace_root(), dir);
    assert_eq!(ed.workspace_notes().len(), 4);

    // A wiki link finds the note anywhere in the workspace.
    keys(&mut ed, "jjfP<CR>");
    assert_eq!(ed.path.as_deref(), Some(dir.join("deep/er/Plan.md").as_path()));
    assert_eq!(ed.tab_count(), 1, "a clean document is replaced in place");
    // Ctrl-o returns to where the link was followed; Ctrl-i goes forward again.
    keys(&mut ed, "<C-o>");
    assert_eq!((ed.file_name().as_str(), ed.buf.line_of(ed.cursor)), ("index.md", 2));
    keys(&mut ed, "<C-i>");
    assert_eq!(ed.file_name(), "Plan.md");
    // And the note links back.
    keys(&mut ed, "Gkfigf");
    assert_eq!(ed.file_name(), "index.md");

    // Heading anchors land on the heading.
    keys(&mut ed, "ggjjfIgf");
    assert_eq!(
        (
            ed.file_name().as_str(),
            ed.buf.line_text(ed.buf.line_of(ed.cursor)).as_str()
        ),
        ("Ideas.md", "## Later")
    );
    keys(&mut ed, "<C-o>fggf");
    assert_eq!(ed.file_name(), "guide.md");

    // Backlinks: who links to index.md?
    ed.open_path(&dir.join("index.md")).unwrap();
    let mut from: Vec<String> = ed
        .backlinks()
        .iter()
        .map(|link| link.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    from.sort();
    assert_eq!(from, vec!["Ideas.md", "Plan.md"]);
    keys(&mut ed, ":backlinks<CR>");
    assert_eq!(ed.palette.as_ref().unwrap().matches.len(), 2);
    keys(&mut ed, "plan<CR>");
    assert_eq!((ed.file_name().as_str(), ed.buf.line_of(ed.cursor)), ("Plan.md", 2));

    // A link to a note that does not exist yet starts it beside this one.
    ed.open_path(&dir.join("index.md")).unwrap();
    keys(&mut ed, "GkfF<CR>");
    assert_eq!(ed.path.as_deref(), Some(dir.join("Fresh Note.md").as_path()));
    assert!(ed.buf.is_empty() && !dir.join("Fresh Note.md").exists());
    keys(&mut ed, "ihello<Esc>:w<CR>");
    assert!(dir.join("Fresh Note.md").exists());

    // With unsaved changes, following a link opens a tab instead of replacing.
    ed.open_path(&dir.join("index.md")).unwrap();
    keys(&mut ed, "ggx jjfP<CR>".replace(' ', "").as_str());
    assert_eq!((ed.tab_count(), ed.file_name().as_str()), (2, "Plan.md"));
    keys(&mut ed, "[t");
    assert_eq!(ed.file_name(), "index.md");

    // URLs and non-text files go to the system.
    let mut ed = editor("|see https://example.com and ![p](pic.png)");
    keys(&mut ed, "fhgx");
    assert_eq!(ed.take_effects(), vec![Effect::OpenUrl("https://example.com".into())]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn typing_two_brackets_offers_notes() {
    let dir = temp_dir("complete");
    std::fs::write(dir.join("a.md"), "").unwrap();
    std::fs::write(dir.join("Target Note.md"), "").unwrap();
    let mut ed = editor("");
    ed.open(&dir.join("a.md")).unwrap();
    keys(&mut ed, "isee [[");
    assert_eq!(
        ed.palette.as_ref().map(|palette| palette.kind),
        Some(PaletteKind::Notes)
    );
    keys(&mut ed, "targ<CR>");
    assert_eq!(show(&ed), "see [[Target Note]]|");
    assert_eq!(ed.mode, Mode::Insert);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_menu_lists_runs_and_inserts() {
    let mut ed = editor("|text");
    keys(&mut ed, "<D-/>");
    let palette = ed.palette.as_ref().unwrap();
    assert_eq!(palette.kind, PaletteKind::Help);
    let sections: std::collections::BTreeSet<&str> = palette.items.iter().map(|item| item.section.as_str()).collect();
    assert_eq!(
        sections.into_iter().collect::<Vec<_>>(),
        vec!["Commands", "Markdown", "Vim keys"]
    );
    assert!(
        palette
            .items
            .iter()
            .any(|item| item.label == "Save" && item.detail == "⌘S")
    );
    // Typing filters; keys never reach the document.
    keys(&mut ed, "focus");
    assert_eq!(
        ed.palette.as_ref().unwrap().selected_item().unwrap().label,
        "Focus mode"
    );
    keys(&mut ed, "<CR>");
    assert!(ed.palette.is_none() && ed.config.focus);
    assert_eq!(ed.buf.text(), "text");
    // The same shortcut closes it; Esc too.
    keys(&mut ed, "<D-/><D-/>");
    assert!(ed.palette.is_none());
    keys(&mut ed, ":help<CR>table");
    assert_eq!(
        ed.palette.as_ref().unwrap().selected_item().unwrap().section,
        "Markdown"
    );
    keys(&mut ed, "<CR>");
    assert_eq!(
        ed.buf.text(),
        "text\n| Column | Column |\n|--------|--------|\n|        |        |"
    );
    // Themes preview as the selection moves and revert on Esc.
    let before = ed.theme.name.clone();
    keys(&mut ed, "<Esc>:theme<CR><Down>");
    assert_ne!(ed.theme.name, before);
    keys(&mut ed, "<Esc>");
    assert_eq!(ed.theme.name, before);
    keys(&mut ed, ":theme<CR>nord<CR>");
    assert_eq!(ed.theme.name, "nord");
    // Fonts come from the shell's list.
    ed.font_families = vec!["Georgia".into(), "Menlo".into()];
    ed.take_effects();
    keys(&mut ed, ":font<CR>geo<CR>");
    assert_eq!(ed.config.prose_font[0], "Georgia");
    assert!(ed.take_effects().contains(&Effect::FontChanged));
    keys(&mut ed, ":font Men<Tab><CR>");
    assert_eq!(ed.config.prose_font[..2], ["Menlo".to_string(), "Georgia".to_string()]);
}

#[test]
fn the_file_browser_walks_and_opens() {
    let dir = temp_dir("browser");
    std::fs::create_dir_all(dir.join("notes/sub")).unwrap();
    std::fs::write(dir.join("b.md"), "bee").unwrap();
    std::fs::write(dir.join("a.txt"), "ay").unwrap();
    std::fs::write(dir.join(".hidden"), "").unwrap();
    std::fs::write(dir.join("pic.png"), "").unwrap();
    std::fs::write(dir.join("notes/inner.md"), "inner").unwrap();
    let mut ed = editor("");
    ed.open(&dir.join("b.md")).unwrap();
    assert!(!ed.sidebar.visible);
    keys(&mut ed, "<D-\\>");
    assert!(ed.sidebar.visible && ed.sidebar.focused);
    let names = |ed: &Editor| {
        ed.sidebar
            .entries
            .iter()
            .map(|entry| format!("{}{}", "  ".repeat(entry.depth), entry.name))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&ed), vec!["notes", "a", "b", "pic.png"]);
    assert_eq!(
        ed.sidebar.entries[ed.sidebar.selected].name, "b",
        "the open document is selected"
    );
    // Keys drive the tree, not the text.
    keys(&mut ed, "ggl");
    assert_eq!(names(&ed), vec!["notes", "  sub", "  inner", "a", "b", "pic.png"]);
    keys(&mut ed, "jj<CR>");
    assert_eq!((ed.file_name().as_str(), ed.buf.text().as_str()), ("inner.md", "inner"));
    assert!(ed.sidebar.visible && !ed.sidebar.focused, "focus returns to the text");
    keys(&mut ed, "x");
    assert_eq!(ed.buf.text(), "nner");
    // With unsaved changes a click opens a tab; the tree stays.
    let row = ed.sidebar.entries.iter().position(|entry| entry.name == "a").unwrap();
    ed.sidebar_click(row, false);
    assert_eq!((ed.tab_count(), ed.buf.text().as_str()), (2, "ay"));
    // h collapses, - widens to the parent folder, the toggle hides.
    keys(&mut ed, "<D-\\>ggh");
    assert_eq!(names(&ed).len(), 4);
    keys(&mut ed, "-");
    assert_eq!(ed.sidebar.root, dir.parent().unwrap());
    keys(&mut ed, "<D-\\>");
    assert!(!ed.sidebar.visible);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unsaved_text_survives_a_crash() {
    let dir = temp_dir("recovery");
    let file = dir.join("doc.md");
    std::fs::write(&file, "saved\n").unwrap();
    let recovery = dir.join("recovery");
    let crashed = |recovery: &std::path::Path| {
        let mut ed = editor("");
        ed.recovery_dir = Some(recovery.to_path_buf());
        ed
    };
    let mut ed = crashed(&recovery);
    ed.recover_in_background(true);
    ed.open(&file).unwrap();
    ed.write_recovery();
    ed.flush_recovery();
    assert!(
        !recovery.exists() || std::fs::read_dir(&recovery).unwrap().count() == 0,
        "clean documents leave nothing"
    );
    keys(&mut ed, "Aand unsaved<Esc>");
    ed.write_recovery();
    ed.new_tab();
    keys(&mut ed, "ia draft<Esc>");
    ed.write_recovery();
    ed.flush_recovery();
    assert_eq!(std::fs::read_dir(&recovery).unwrap().count(), 2);
    drop(ed); // no shutdown: a crash

    let mut ed = crashed(&recovery);
    ed.open(&file).unwrap();
    assert_eq!(ed.buf.text(), "savedand unsaved\n");
    assert!(ed.buf.is_dirty());
    keys(&mut ed, "u");
    assert_eq!(ed.buf.text(), "saved\n");
    keys(&mut ed, "<C-r>:w<CR>");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "savedand unsaved\n");
    assert_eq!(
        std::fs::read_dir(&recovery).unwrap().count(),
        1,
        "saving removes the snapshot"
    );

    // Untitled drafts from another process come back as tabs.
    let draft = std::fs::read_dir(&recovery).unwrap().next().unwrap().unwrap().path();
    std::fs::rename(&draft, recovery.join("untitled-1-1.md")).unwrap();
    ed.recover_untitled();
    assert_eq!(ed.tab_count(), 2);
    ed.switch_tab(1);
    assert_eq!((ed.buf.text().as_str(), ed.buf.is_dirty()), ("a draft", true));
    // An orderly exit discards what the user chose not to save.
    ed.shutdown();
    assert_eq!(std::fs::read_dir(&recovery).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn code_blocks_are_highlighted_by_language() {
    use crate::markdown::syntax;
    let mut ed = editor(
        "---\ntitle: x\n---\n\n```rust\nfn main() {}\n```\n\n> ```py\n> def f(): pass\n> ```\n\n```nope\nplain\n```\n",
    );
    ed.refresh();
    let kinds = |ed: &Editor, line: usize| {
        let text = ed.buf.line_text(line);
        let (lines, at) = ed.code_spans(line)?;
        Some(
            lines[at]
                .iter()
                .map(|span| (text[span.start as usize..span.end as usize].to_string(), span.syntax))
                .collect::<Vec<_>>(),
        )
    };
    assert!(kinds(&ed, 5).unwrap().contains(&("fn".to_string(), syntax::KEYWORD)));
    assert!(kinds(&ed, 1).unwrap().contains(&("title".to_string(), syntax::TAG)));
    assert!(
        kinds(&ed, 9).unwrap().contains(&("def".to_string(), syntax::KEYWORD)),
        "nested blocks skip the quote prefix"
    );
    assert_eq!(kinds(&ed, 13), None);
    assert_eq!(kinds(&ed, 4), None, "fence lines are markdown");
    keys(&mut ed, "5Gjcwlet<Esc>");
    ed.refresh();
    assert!(kinds(&ed, 5).unwrap().contains(&("let".to_string(), syntax::KEYWORD)));
    ed.config.highlight = false;
    assert_eq!(kinds(&ed, 5), None);
}

#[test]
fn highlighting_never_leaks_between_documents() {
    let dir = temp_dir("highlight-switch");
    std::fs::write(dir.join("a.md"), "```rust\nfn a() {}\nfn b() {}\nfn c() {}\n```\n").unwrap();
    std::fs::write(dir.join("b.md"), "```rust\nlet only = 1;\n```\n").unwrap();
    let mut ed = editor("");
    ed.open(&dir.join("a.md")).unwrap();
    ed.refresh();
    assert_eq!(ed.code_spans(3).map(|(lines, at)| (lines.len(), at)), Some((3, 2)));
    // Both buffers are at revision 0; the cache must not carry over.
    ed.open(&dir.join("b.md")).unwrap();
    ed.refresh();
    assert_eq!(ed.code_spans(1).map(|(lines, at)| (lines.len(), at)), Some((1, 0)));
    ed.new_tab();
    ed.refresh();
    assert!(ed.code_spans(1).is_none());
    ed.switch_tab(0);
    ed.refresh();
    assert_eq!(ed.code_spans(1).map(|(lines, _)| lines.len()), Some(1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_context_menu_acts_on_what_is_under_the_cursor() {
    let mut ed = editor("see {++more++}{>>id:s_0123abcd by:Claude<<} and [[Other]] - [ ] x");
    let labels = |ed: &Editor| {
        ed.context_menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|item| (item.label.clone(), item.key))
            .collect::<Vec<_>>()
    };
    // On a suggestion: K opens it, `a` accepts.
    keys(&mut ed, "fmK");
    assert_eq!(
        labels(&ed)[..2],
        [
            ("Accept Claude's suggestion".to_string(), Some('a')),
            ("Reject it".to_string(), Some('r'))
        ]
    );
    keys(&mut ed, "a");
    assert!(ed.context_menu.is_none());
    assert_eq!(ed.buf.text(), "see more and [[Other]] - [ ] x");
    // Keys go to the menu, not the text; j/k move, Esc closes.
    keys(&mut ed, "ugmjjk");
    assert_eq!(ed.context_menu.as_ref().unwrap().selected, 1);
    keys(&mut ed, "<CR>");
    assert_eq!(ed.buf.text(), "see  and [[Other]] - [ ] x", "the second item rejects");
    keys(&mut ed, "K<Esc>x");
    assert_eq!(ed.buf.text(), "see and [[Other]] - [ ] x");
    // A right-click moves the cursor to the link and offers to follow it.
    ed.context_menu_at(12, 40.0, 50.0);
    assert_eq!(ed.cursor, 12);
    assert_eq!(ed.context_menu.as_ref().unwrap().at, MenuAt::Point(40.0, 50.0));
    assert!(
        labels(&ed)
            .iter()
            .any(|(label, key)| label == "Go to \"Other\"" && *key == Some('f'))
    );
    // A right-click inside a selection keeps it and offers editing.
    keys(&mut ed, "<Esc>0ve");
    ed.context_menu_at(1, 0.0, 0.0);
    assert_eq!(ed.selection(), Some(0..3));
    keys(&mut ed, "y");
    keys(&mut ed, "<Esc>$p");
    assert_eq!(ed.buf.text(), "see and [[Other]] - [ ] xsee");
    // Letters never collide with the navigation keys.
    keys(&mut ed, "K");
    let menu = ed.context_menu.as_ref().unwrap();
    assert!(
        menu.items
            .iter()
            .all(|item| item.key.is_none_or(|key| !"jkhlq".contains(key)))
    );
    let mut seen = std::collections::HashSet::new();
    assert!(
        menu.items
            .iter()
            .filter_map(|item| item.key)
            .all(|key| seen.insert(key))
    );
}

fn editor_with_keys(text: &str, toml: &str) -> Editor {
    let mut config = Config::from_toml(toml).unwrap();
    config.visual_line_motion = false;
    let mut ed = Editor::new(config, Box::new(MemoryClipboard::default()));
    ed.primary = Primary::Super;
    let cursor = text.find('|').map_or(0, |at| text[..at].chars().count());
    ed.set_text(&text.replacen('|', "", 1));
    ed.cursor = cursor;
    ed
}

#[test]
fn key_bindings_can_all_be_overridden() {
    let toml = r#"
        [keys.normal]
        "H" = "^"
        "<Space>w" = ":suggest on"
        "<Space>x" = "dd"
        "gt" = "<Nop>"
        "K" = ":focus"
        "dd" = "x"
        [keys.insert]
        "jk" = "<Esc>"
        [keys.visual]
        "s" = "d"
        [keys.all]
        "<D-s>" = ":typewriter"
    "#;
    let run = |text: &str, notation: &str| {
        let mut ed = editor_with_keys(text, toml);
        keys(&mut ed, notation);
        (show(&ed), ed)
    };
    assert_eq!(run("  ab|c", "H").0, "  |abc");
    let (_, ed) = run("|x", "<Space>w");
    assert!(ed.suggesting);
    assert_eq!(
        run("one\n|two\nthree", "<Space>x").0,
        "one\n|three",
        "bindings run built-in keys, not other bindings"
    );
    // A built-in can be switched off or replaced, shortcuts included.
    assert_eq!(run("- [ ] |a", "gt").0, "- [ ] |a");
    let (_, ed) = run("|x", "K<D-s>");
    assert!(ed.context_menu.is_none() && ed.config.focus && ed.config.typewriter && !ed.buf.is_dirty());
    assert_eq!(run("|abc\ndef", "dd").0, "|bc\ndef");
    // A key that only starts a binding falls through to the built-in when the rest does not follow.
    assert_eq!(run("|abc\ndef", "dw").0, "|\ndef");
    assert_eq!(run("a |b c", "<Space>l").0, "a b |c");
    // Insert mode types as it goes and takes the keys back when the sequence completes.
    let (text, ed) = run("|", "ihej jjk");
    assert_eq!((text.as_str(), ed.mode), ("hej |j", Mode::Normal));
    assert_eq!(run("|", "ijak<Esc>").0, "ja|k");
    assert_eq!(run("|abc def", "ves").0, "| def");
    // Bindings stay out of prompts and menus.
    let (_, ed) = run("|x", ":jk");
    assert_eq!(ed.cmdline.as_ref().unwrap().text, "jk");
    assert!(Config::from_toml("[keys.nope]\n\"a\" = \"b\"").is_err());
}

#[test]
fn other_programs_edit_without_disturbing_the_writer() {
    let dir = temp_dir("remote");
    std::fs::write(dir.join("a.md"), "alpha beta gamma\nsecond line\n").unwrap();
    std::fs::write(dir.join("b.md"), "bee\n").unwrap();
    let mut ed = editor("");
    ed.open(&dir.join("a.md")).unwrap();
    ed.open_in_tab(&dir.join("b.md")).unwrap();
    ed.switch_tab(0);
    let suggest = |target: Target, text: &str| RemoteEdit {
        target,
        text: text.to_string(),
        suggest: true,
        author: "Claude".to_string(),
    };
    let direct = |target: Target, text: &str| RemoteEdit {
        suggest: false,
        ..suggest(target, text)
    };
    let old = |text: &str| Target::Text {
        old: text.to_string(),
        occurrence: None,
    };

    let sessions = ed.sessions();
    assert_eq!(
        sessions
            .iter()
            .map(|s| (s.title.as_str(), s.active, s.lines))
            .collect::<Vec<_>>(),
        vec![("a.md", true, 3), ("b.md", false, 2)]
    );
    assert_eq!(ed.find_session("b.md"), Ok(1));
    assert_eq!(ed.find_session("2"), Ok(1));
    assert_eq!(ed.find_session(&dir.join("a.md").to_string_lossy()), Ok(0));
    assert_eq!(ed.find_session(""), Ok(0));
    assert!(ed.find_session("zzz").is_err());

    // The writer is mid-word in insert mode; an edit elsewhere leaves them there.
    keys(&mut ed, "wwiX");
    assert_eq!(show(&ed), "alpha beta X|gamma\nsecond line\n");
    let read = ed.snapshot(0);
    assert_eq!(
        (read.text.as_str(), read.cursor),
        ("alpha beta Xgamma\nsecond line\n", 12)
    );
    let applied = ed.remote_edit(0, &suggest(old("alpha"), "ALPHA")).unwrap();
    assert_eq!(applied.line, 0);
    assert_eq!(ed.mode, Mode::Insert);
    keys(&mut ed, "Y");
    let text = ed.buf.text();
    assert!(
        text.starts_with("{~~alpha~>ALPHA~~}{>>id:s_")
            && text.contains(" by:Claude<<} beta XY|gamma".replace('|', "").as_str()),
        "{text}"
    );
    assert_eq!(ed.config.author, "Calvin", "the writer's own name is untouched");
    // Undo takes back their typing and the suggestion as separate steps.
    keys(&mut ed, "<Esc>u");
    assert!(ed.buf.text().contains("beta Xgamma") && ed.buf.text().contains("{~~alpha"));
    keys(&mut ed, "u");
    assert_eq!(ed.buf.text(), "alpha beta Xgamma\nsecond line\n");

    // A range read earlier follows what was typed since.
    let read = ed.snapshot(0);
    let second = read.text.find("second").unwrap();
    keys(&mut ed, "ggI>> <Esc>");
    let range = Target::Range {
        revision: read.session.revision,
        range: second..second + 6,
    };
    ed.remote_edit(0, &direct(range.clone(), "2nd")).unwrap();
    assert_eq!(ed.buf.text(), ">> alpha beta Xgamma\n2nd line\n");
    assert!(
        ed.remote_edit(0, &direct(range, "again"))
            .unwrap_err()
            .contains("changed"),
        "the same range is now stale"
    );

    // Text targets must be unambiguous and present.
    assert!(ed.remote_edit(0, &direct(old("a"), "b")).unwrap_err().contains("times"));
    assert!(
        ed.remote_edit(0, &direct(old("nowhere"), "b"))
            .unwrap_err()
            .contains("not in the document")
    );
    ed.remote_edit(
        0,
        &direct(
            Target::Text {
                old: "a".into(),
                occurrence: Some(2),
            },
            "A",
        ),
    )
    .unwrap();
    assert!(ed.buf.text().starts_with(">> alphA beta"));

    // A background tab is edited in place; the visible one does not flicker.
    keys(&mut ed, ":");
    let cursor = ed.cursor;
    ed.remote_edit(1, &direct(Target::End, "added\n")).unwrap();
    assert_eq!((ed.active, ed.cursor, ed.cmdline.is_some()), (0, cursor, true));
    assert_eq!(ed.snapshot(1).text, "bee\nadded\n");
    assert!(ed.tabs()[1].dirty);
    assert!(ed.message.as_ref().unwrap().text.contains("in b.md"));
    keys(&mut ed, "<Esc>");

    // Inserting at the cursor leaves the writer before the new text.
    keys(&mut ed, "G");
    ed.remote_edit(0, &direct(Target::Cursor, "here ")).unwrap();
    assert_eq!(ed.buf.line_of(ed.cursor), ed.buf.line_count() - 1);
    ed.remote_edit(0, &direct(Target::Line(0), "# Title\n\n")).unwrap();
    assert!(ed.buf.text().starts_with("# Title\n\n>> alphA"));
    // What a suggestion cannot hold is refused, and nothing changes.
    let before = ed.buf.text();
    assert!(ed.remote_edit(0, &suggest(old("beta"), "x {++ y")).is_err());
    assert!(
        ed.remote_edit(
            0,
            &RemoteEdit {
                author: "a<<}b".into(),
                ..suggest(old("beta"), "x")
            }
        )
        .is_err()
    );
    assert_eq!(ed.buf.text(), before);
    let _ = std::fs::remove_dir_all(dir);
}
