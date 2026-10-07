//! Syntax highlighting for code blocks, using Sublime Text grammars through
//! `syntect`. Results are cached per block and re-highlighted incrementally:
//! an edit only re-parses from the changed line until the parser state
//! converges with the previous run.

use crate::markdown::{Span, syntax};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, LazyLock};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

/// Blocks longer than this stay plain rather than stall a keystroke.
const MAX_LINES: usize = 4000;
const MAX_LINE_BYTES: usize = 2000;
const CACHED_BLOCKS: usize = 48;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Scope prefixes in match order, most specific first.
static KINDS: LazyLock<Vec<(Scope, u8)>> = LazyLock::new(|| {
    [
        ("comment", syntax::COMMENT),
        ("string", syntax::STRING),
        ("constant.numeric", syntax::NUMBER),
        ("constant.character.escape", syntax::OPERATOR),
        ("constant", syntax::CONSTANT),
        ("keyword.operator", syntax::OPERATOR),
        ("keyword", syntax::KEYWORD),
        ("storage", syntax::KEYWORD),
        ("entity.name.function", syntax::FUNCTION),
        ("support.function", syntax::FUNCTION),
        ("variable.function", syntax::FUNCTION),
        ("meta.function-call.identifier", syntax::FUNCTION),
        ("entity.name.tag", syntax::TAG),
        ("entity.other.attribute-name", syntax::ATTRIBUTE),
        ("entity.other.inherited-class", syntax::TYPE),
        ("entity.name", syntax::TYPE),
        ("support.type", syntax::TYPE),
        ("support.class", syntax::TYPE),
        ("support.constant", syntax::CONSTANT),
        ("variable.language", syntax::KEYWORD),
        ("variable.parameter", syntax::VARIABLE),
        ("variable.other.member", syntax::VARIABLE),
        ("meta.mapping.key", syntax::TAG),
        ("markup.inserted", syntax::STRING),
        ("markup.deleted", syntax::TAG),
        ("markup.heading", syntax::KEYWORD),
        ("meta.diff", syntax::FUNCTION),
        ("punctuation", syntax::PUNCTUATION),
    ]
    .iter()
    .map(|(prefix, kind)| (Scope::new(prefix).unwrap(), *kind))
    .collect()
});

/// Loads the grammars on a background thread so the first code block does
/// not pay for it.
pub fn warm() {
    std::thread::spawn(|| {
        LazyLock::force(&SYNTAXES);
        LazyLock::force(&KINDS);
    });
}

/// How many languages are known.
pub fn language_count() -> usize {
    SYNTAXES.syntaxes().len()
}

fn kind_of(stack: &ScopeStack) -> u8 {
    let string_edge = Scope::new("punctuation.definition.string").unwrap();
    let comment_edge = Scope::new("punctuation.definition.comment").unwrap();
    for scope in stack.as_slice().iter().rev() {
        // Quotes and comment leaders take the color of what they delimit.
        if string_edge.is_prefix_of(*scope) || comment_edge.is_prefix_of(*scope) {
            continue;
        }
        if let Some((_, kind)) = KINDS.iter().find(|(prefix, _)| prefix.is_prefix_of(*scope)) {
            return *kind;
        }
    }
    0
}

fn alias(lang: &str) -> &str {
    match lang {
        "shell" | "zsh" | "console" | "shellsession" | "fish" => "sh",
        "jsonc" | "json5" => "json",
        "c++" => "cpp",
        "golang" => "go",
        "node" | "javascript" | "mjs" => "js",
        "typescript" => "ts",
        "python3" => "py",
        "rust" => "rs",
        "yml" => "yaml",
        "viml" => "vim",
        "objective-c" | "objc" => "m",
        "c#" | "csharp" => "cs",
        other => other,
    }
}

pub type Lines = Arc<Vec<Vec<Span>>>;

struct Cached {
    lang: String,
    hashes: Vec<u64>,
    /// Parser state after each line.
    states: Vec<(ParseState, ScopeStack)>,
    spans: Lines,
    used: u64,
}

#[derive(Default)]
pub struct Highlighter {
    recent: Vec<Cached>,
    clock: u64,
}

impl Highlighter {
    /// Token spans for each line of a block, relative to the line's start.
    /// `None` when the language is unknown.
    pub fn block(&mut self, lang: &str, lines: &[&str]) -> Option<Lines> {
        if lines.len() > MAX_LINES {
            return None;
        }
        let syntax = SYNTAXES.find_syntax_by_token(alias(lang))?;
        self.clock += 1;
        let hashes: Vec<u64> = lines
            .iter()
            .map(|line| {
                let mut hasher = DefaultHasher::new();
                line.hash(&mut hasher);
                hasher.finish()
            })
            .collect();
        let common = |cached: &Cached| cached.hashes.iter().zip(&hashes).take_while(|(a, b)| a == b).count();
        let previous = self
            .recent
            .iter()
            .enumerate()
            .filter(|(_, cached)| cached.lang == lang)
            .max_by_key(|(_, cached)| common(cached))
            .map(|(index, cached)| (index, common(cached)));
        if let Some((index, shared)) = previous
            && shared == hashes.len() && self.recent[index].hashes.len() == hashes.len() {
                self.recent[index].used = self.clock;
                return Some(self.recent[index].spans.clone());
            }

        let mut states: Vec<(ParseState, ScopeStack)> = Vec::with_capacity(lines.len());
        let mut spans: Vec<Vec<Span>> = Vec::with_capacity(lines.len());
        let mut tail = None;
        if let Some((index, shared)) = previous {
            let old = &self.recent[index];
            states.extend_from_slice(&old.states[..shared]);
            spans.extend_from_slice(&old.spans[..shared]);
            // Lines after the edit that are unchanged, counted from the end.
            let suffix = old.hashes[shared..]
                .iter()
                .rev()
                .zip(hashes[shared..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            tail = Some((index, suffix));
        }
        let (mut parser, mut stack) = states.last().cloned().unwrap_or((ParseState::new(syntax), ScopeStack::new()));
        let mut line = states.len();
        while line < lines.len() {
            let text = lines[line];
            let mut out = Vec::new();
            if text.len() <= MAX_LINE_BYTES {
                let source = format!("{text}\n");
                let ops = parser.parse_line(&source, &SYNTAXES).unwrap_or_default();
                let mut at = 0;
                let push = |from: usize, to: usize, stack: &ScopeStack, out: &mut Vec<Span>| {
                    let to = to.min(text.len());
                    let kind = kind_of(stack);
                    if kind != 0 && from < to {
                        match out.last_mut() {
                            Some(last) if last.syntax == kind && last.end as usize == from => last.end = to as u32,
                            _ => out.push(Span { start: from as u32, end: to as u32, style: 0, syntax: kind }),
                        }
                    }
                };
                for (offset, op) in ops {
                    push(at, offset, &stack, &mut out);
                    at = offset.max(at);
                    let _ = stack.apply(&op);
                }
                push(at, text.len(), &stack, &mut out);
            }
            spans.push(out);
            states.push((parser.clone(), stack.clone()));
            line += 1;
            // Converged with the previous run: the rest is unchanged.
            if let Some((index, suffix)) = tail {
                let old = &self.recent[index];
                let remaining = lines.len() - line;
                if remaining <= suffix && remaining > 0 {
                    let old_line = old.hashes.len() - remaining;
                    if old_line > 0 && old.states[old_line - 1] == states[line - 1] {
                        states.extend_from_slice(&old.states[old_line..]);
                        spans.extend_from_slice(&old.spans[old_line..]);
                        break;
                    }
                }
            }
        }

        let spans: Lines = Arc::new(spans);
        if self.recent.len() >= CACHED_BLOCKS
            && let Some(oldest) = (0..self.recent.len()).min_by_key(|&index| self.recent[index].used) {
                self.recent.swap_remove(oldest);
            }
        self.recent.push(Cached {
            lang: lang.to_string(),
            hashes,
            states,
            spans: spans.clone(),
            used: self.clock,
        });
        Some(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &str, source: &str) -> Vec<Vec<(String, u8)>> {
        let lines: Vec<&str> = source.lines().collect();
        let spans = Highlighter::default().block(lang, &lines).expect("known language");
        lines
            .iter()
            .zip(spans.iter())
            .map(|(line, spans)| {
                spans.iter().map(|span| (line[span.start as usize..span.end as usize].to_string(), span.syntax)).collect()
            })
            .collect()
    }

    #[test]
    fn highlights_common_languages() {
        let rust = kinds("rust", "fn main() {\n    let x = \"hi\"; // note\n}");
        assert!(rust[0].contains(&("fn".to_string(), syntax::KEYWORD)));
        assert!(rust[0].contains(&("main".to_string(), syntax::FUNCTION)));
        assert!(rust[1].contains(&("\"hi\"".to_string(), syntax::STRING)));
        assert!(rust[1].contains(&("// note".to_string(), syntax::COMMENT)));
        for lang in ["python", "ts", "typescript", "go", "json", "yaml", "toml", "sh", "bash", "sql", "html", "css", "c", "cpp", "java", "kotlin", "swift", "ruby", "dockerfile", "diff", "lua", "zig", "nix", "elixir", "haskell"] {
            assert!(Highlighter::default().block(lang, &["x = 1"]).is_some(), "{lang}");
        }
        assert!(Highlighter::default().block("no-such-language", &["x"]).is_none());
        assert!(language_count() > 150);
    }

    #[test]
    fn incremental_runs_match_a_fresh_parse() {
        let before = "/* open\nstill comment */\nlet a = 1;\nlet b = \"two\";\nfn f() {}";
        let mut highlighter = Highlighter::default();
        let lines: Vec<&str> = before.lines().collect();
        highlighter.block("rust", &lines);
        for after in [
            "/* open\nstill comment */\nlet a = 12;\nlet b = \"two\";\nfn f() {}",
            "/* open\nstill comment\nlet a = 1;\nlet b = \"two\";\nfn f() {}",
            "/* open\nstill comment */\nlet b = \"two\";\nfn f() {}",
            "// x\n/* open\nstill comment */\nlet a = 1;\nextra\nlet b = \"two\";\nfn f() {}",
        ] {
            let lines: Vec<&str> = after.lines().collect();
            let incremental = highlighter.block("rust", &lines).unwrap();
            let fresh = Highlighter::default().block("rust", &lines).unwrap();
            assert_eq!(incremental, fresh, "{after}");
        }
    }
}
