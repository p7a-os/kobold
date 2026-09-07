//! Minimal syntax highlighting driven by the fence language hint.
//!
//! Deliberately not syntect: that means Sublime syntax definitions, a regex
//! engine, and megabytes of binary to render a handful of short snippets in a
//! terminal. A generic lexer -- comments, strings, numbers, keywords, call
//! names -- captures nearly all of the legibility benefit at this size, and
//! degrades to plain text for anything it does not know.

use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tok {
    Plain,
    Comment,
    Str,
    Num,
    Keyword,
    Func,
}

impl Tok {
    /// Muted 256-colour palette: readable on the tinted slab without turning
    /// a code block into the loudest thing on screen.
    pub fn color(self) -> Color {
        match self {
            Tok::Plain => Color::Indexed(252),
            Tok::Comment => Color::Indexed(243),
            Tok::Str => Color::Indexed(108),
            Tok::Num => Color::Indexed(173),
            Tok::Keyword => Color::Indexed(176),
            Tok::Func => Color::Indexed(110),
        }
    }
}

struct Lang {
    line_comment: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    /// Python-style triple quotes, checked before single quotes.
    triple: bool,
    keywords: &'static [&'static str],
}

const C_LIKE: Option<(&str, &str)> = Some(("/*", "*/"));

fn lang(hint: &str) -> Option<Lang> {
    let h = hint.trim().to_ascii_lowercase();
    let h = h.split_whitespace().next().unwrap_or("");
    Some(match h {
        "rust" | "rs" => Lang {
            line_comment: &["//"],
            block_comment: C_LIKE,
            quotes: &['"', '\''],
            triple: false,
            keywords: &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
        },
        "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx" => Lang {
            line_comment: &["//"],
            block_comment: C_LIKE,
            quotes: &['"', '\'', '`'],
            triple: false,
            keywords: &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "from",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "interface",
                "let",
                "new",
                "null",
                "of",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "type",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "yield",
            ],
        },
        "python" | "py" => Lang {
            line_comment: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            triple: true,
            keywords: &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise",
                "return", "True", "try", "while", "with", "yield",
            ],
        },
        "go" => Lang {
            line_comment: &["//"],
            block_comment: C_LIKE,
            quotes: &['"', '`'],
            triple: false,
            keywords: &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "type",
                "var",
                "nil",
                "true",
                "false",
            ],
        },
        "c" | "cpp" | "c++" | "java" | "cs" | "csharp" => Lang {
            line_comment: &["//"],
            block_comment: C_LIKE,
            quotes: &['"', '\''],
            triple: false,
            keywords: &[
                "auto",
                "bool",
                "break",
                "case",
                "catch",
                "char",
                "class",
                "const",
                "continue",
                "default",
                "delete",
                "do",
                "double",
                "else",
                "enum",
                "extern",
                "false",
                "final",
                "float",
                "for",
                "if",
                "import",
                "int",
                "long",
                "namespace",
                "new",
                "null",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "short",
                "sizeof",
                "static",
                "struct",
                "switch",
                "template",
                "this",
                "throw",
                "true",
                "try",
                "typedef",
                "union",
                "unsigned",
                "using",
                "virtual",
                "void",
                "while",
            ],
        },
        "bash" | "sh" | "shell" | "zsh" => Lang {
            line_comment: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            triple: false,
            keywords: &[
                "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function",
                "if", "in", "local", "return", "then", "until", "while",
            ],
        },
        "sql" => Lang {
            line_comment: &["--"],
            block_comment: C_LIKE,
            quotes: &['"', '\''],
            triple: false,
            keywords: &[
                "and", "as", "asc", "by", "create", "delete", "desc", "distinct", "drop", "from",
                "group", "having", "insert", "into", "join", "left", "limit", "not", "null", "on",
                "or", "order", "select", "set", "table", "update", "values", "where",
            ],
        },
        "json" => Lang {
            line_comment: &[],
            block_comment: None,
            quotes: &['"'],
            triple: false,
            keywords: &["true", "false", "null"],
        },
        "yaml" | "yml" | "toml" | "ini" => Lang {
            line_comment: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            triple: false,
            keywords: &["true", "false", "null", "yes", "no"],
        },
        _ => return None,
    })
}

/// What is still open at the end of a line.
///
/// A block comment or a multi-line string does not end at a newline, so a line
/// cannot be tokenised on its own without knowing what ran into it. Keeping
/// that as an explicit, cheap value is what lets a fence still being streamed
/// tokenise only its newest line instead of all of it again.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Carry {
    /// Ordinary code.
    #[default]
    Code,
    /// Inside a block comment, waiting for the language's closing marker.
    Comment,
    /// Inside a triple-quoted string, waiting for `delim`.
    Triple(&'static str),
    /// Inside an ordinary string opened with this quote and never closed.
    /// Matches the whole-block lexer, which runs an unterminated string to the
    /// end of the input rather than guessing that the author meant a newline.
    Quote(char),
}

/// Tokenise `code` into one token list per line. Unknown languages, and blocks
/// with no hint at all, come back as a single plain token per line.
pub fn highlight(hint: &str, code: &str) -> Vec<Vec<(String, Tok)>> {
    let mut carry = Carry::Code;
    code.split('\n')
        .map(|line| {
            let (toks, next) = highlight_line(hint, line, carry);
            carry = next;
            toks
        })
        .collect()
}

/// Tokenise one line, given what the line before it left open, and report what
/// this one leaves open in turn.
pub fn highlight_line(hint: &str, line: &str, carry: Carry) -> (Vec<(String, Tok)>, Carry) {
    let Some(l) = lang(hint) else {
        return (vec![(line.to_owned(), Tok::Plain)], Carry::Code);
    };

    let mut out: Vec<(String, Tok)> = Vec::new();
    let mut i = 0usize;
    let mut carry = carry;

    // Finish whatever ran in from the line above. If its closing marker is not
    // on this line either, the whole line takes that colour and the state
    // carries on to the next.
    let opened = match carry {
        Carry::Code => None,
        Carry::Comment => l
            .block_comment
            .map(|(_, close)| (close.to_owned(), Tok::Comment)),
        Carry::Triple(delim) => Some((delim.to_owned(), Tok::Str)),
        Carry::Quote(q) => Some((q.to_string(), Tok::Str)),
    };
    if let Some((close, tok)) = opened {
        // An unescaped closing quote ends a string; a comment marker is literal.
        let end = match carry {
            Carry::Quote(q) => closing_quote(line, q, 0),
            _ => line.find(&close).map(|e| e + close.len()),
        };
        match end {
            Some(e) => {
                push(&mut out, &line[..e], tok);
                carry = Carry::Code;
                i = e;
            }
            None => {
                push(&mut out, line, tok);
                return (out, carry);
            }
        }
    }

    while i < line.len() {
        let rest = &line[i..];

        if l.line_comment.iter().any(|m| rest.starts_with(*m)) {
            push(&mut out, rest, Tok::Comment);
            break;
        }
        if let Some((open, close)) = l.block_comment {
            if let Some(body) = rest.strip_prefix(open) {
                match body.find(close) {
                    Some(e) => {
                        let end = e + open.len() + close.len();
                        push(&mut out, &rest[..end], Tok::Comment);
                        i += end;
                    }
                    None => {
                        push(&mut out, rest, Tok::Comment);
                        carry = Carry::Comment;
                        break;
                    }
                }
                continue;
            }
        }
        if l.triple {
            if let Some(q) = ["\"\"\"", "'''"].into_iter().find(|q| rest.starts_with(q)) {
                match rest[3..].find(q) {
                    Some(e) => {
                        let end = e + 6;
                        push(&mut out, &rest[..end], Tok::Str);
                        i += end;
                    }
                    None => {
                        push(&mut out, rest, Tok::Str);
                        carry = Carry::Triple(q);
                        break;
                    }
                }
                continue;
            }
        }
        let ch = rest.chars().next().expect("non-empty");
        if l.quotes.contains(&ch) {
            match closing_quote(rest, ch, ch.len_utf8()) {
                Some(end) => {
                    push(&mut out, &rest[..end], Tok::Str);
                    i += end;
                }
                None => {
                    push(&mut out, rest, Tok::Str);
                    carry = Carry::Quote(ch);
                    break;
                }
            }
            continue;
        }
        if ch.is_ascii_digit() {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_'))
                .unwrap_or(rest.len());
            push(&mut out, &rest[..end], Tok::Num);
            i += end;
            continue;
        }
        if ch.is_alphabetic() || ch == '_' {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let tok = if l.keywords.contains(&word) {
                Tok::Keyword
            } else if rest[end..].starts_with('(') {
                Tok::Func
            } else {
                Tok::Plain
            };
            push(&mut out, word, tok);
            i += end;
            continue;
        }

        push(&mut out, &rest[..ch.len_utf8()], Tok::Plain);
        i += ch.len_utf8();
    }
    (out, carry)
}

/// Merge into the previous token when the colour matches, so a line of code is
/// a handful of spans rather than one per character. The whole-block lexer used
/// to emit adjacent same-colour runs separately; coalescing here means fewer
/// allocations and fewer cells to style.
fn push(out: &mut Vec<(String, Tok)>, text: &str, tok: Tok) {
    if text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some((prev, t)) if *t == tok => prev.push_str(text),
        _ => out.push((text.to_owned(), tok)),
    }
}

/// Index just past the closing quote, honouring backslash escapes, or `None`
/// when the string does not close on this line -- which is what a half-streamed
/// block looks like, and is carried to the next line rather than guessed at.
///
/// `from` is where scanning starts: past the opening quote when the string
/// opens here, and 0 when resuming inside one that opened on an earlier line,
/// where the very first character may be what closes it.
fn closing_quote(rest: &str, quote: char, from: usize) -> Option<usize> {
    let mut it = rest.char_indices().filter(|(i, _)| *i >= from);
    while let Some((idx, c)) = it.next() {
        if c == '\\' {
            it.next();
            continue;
        }
        if c == quote {
            return Some(idx + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colours of a line, as one token per distinct run.
    fn toks(hint: &str, code: &str) -> Vec<Vec<Tok>> {
        highlight(hint, code)
            .into_iter()
            .map(|line| line.into_iter().map(|(_, t)| t).collect())
            .collect()
    }

    #[test]
    fn a_block_comment_keeps_its_colour_across_lines() {
        // The reason the lexer carries state at all: without it the second and
        // third lines here revert to being read as code.
        let got = toks("rust", "let a = 1;\n/* one\ntwo\nthree */\nlet b = 2;");
        assert_eq!(
            got[1],
            vec![Tok::Comment],
            "comment opens and runs to the line end"
        );
        assert_eq!(
            got[2],
            vec![Tok::Comment],
            "a line wholly inside the comment"
        );
        assert_eq!(got[3], vec![Tok::Comment], "the line that closes it");
        assert_eq!(
            got[4].first(),
            Some(&Tok::Keyword),
            "code resumes after the close"
        );
    }

    #[test]
    fn a_triple_quoted_string_keeps_its_colour_across_lines() {
        let got = toks("python", "x = \"\"\"one\ntwo\n\"\"\"\ny = 1");
        assert_eq!(got[1], vec![Tok::Str], "inside the triple quote");
        assert_eq!(got[2], vec![Tok::Str], "the closing delimiter");
        assert!(got[3].contains(&Tok::Num), "code resumes after it");
    }

    #[test]
    fn an_unterminated_string_runs_on_rather_than_ending_at_the_newline() {
        // Matches what the whole-block lexer did, and is what a half-streamed
        // line should look like: the colour runs on until it is closed.
        let got = toks(
            "rust",
            "let s = \"never closed\nstill string\nend\"; let x = 1;",
        );
        assert_eq!(got[1], vec![Tok::Str]);
        assert!(
            got[2].contains(&Tok::Keyword),
            "the closing quote hands back to code"
        );
    }

    #[test]
    fn resuming_a_line_gives_what_the_whole_block_gives() {
        // `highlight` is defined as this fold, so the point is that a caller
        // driving it line by line -- which is what a streaming fence does --
        // gets identical output to one that had the whole block up front.
        let code = "fn f() {\n    /* c\n    */ let s = \"a\";\n    let n = 42;\n}";
        let whole = toks("rust", code);
        let mut carry = Carry::Code;
        let mut piecewise = Vec::new();
        for line in code.split('\n') {
            let (t, next) = highlight_line("rust", line, carry);
            carry = next;
            piecewise.push(t.into_iter().map(|(_, k)| k).collect::<Vec<_>>());
        }
        assert_eq!(whole, piecewise);
    }

    #[test]
    fn an_unknown_language_is_left_alone() {
        let got = toks("brainfuck", "+++[->+++<]\n>.");
        assert_eq!(got, vec![vec![Tok::Plain], vec![Tok::Plain]]);
    }

    #[test]
    fn adjacent_runs_of_one_colour_become_one_token() {
        // Fewer spans per line is fewer allocations and fewer cells to style,
        // and the whole-block lexer used to emit these separately.
        let (line, _) = highlight_line("rust", "a + b - c", Carry::Code);
        assert_eq!(line.len(), 1, "all plain, so one run: {line:?}");
        assert_eq!(line[0].0, "a + b - c");
    }
}
