//! The executor's output format ("find this text, replace it with that")
//! and the code that applies it. Nothing here calls a model.
//!
//! ```text
//! <<<<<<< SEARCH
//! exact text from the file
//! =======
//! replacement text
//! >>>>>>> REPLACE
//! ```
//!
//! or, when the model lacks information: `MISSING: what it needs`.

use crate::Message;

const SEARCH: &str = "<<<<<<< SEARCH";
const DIVIDER: &str = "=======";
const REPLACE: &str = ">>>>>>> REPLACE";
const MISSING: &str = "MISSING:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub search: String,
    pub replace: String,
}

/// What the model answered, once understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Edits(Vec<Edit>),
    /// The model used the stop instruction instead of guessing.
    Missing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// No complete edit block and no MISSING line.
    Unreadable,
    /// Edit number `index` (from 1): its search text is not in the file.
    NotFound { index: usize },
    /// Edit number `index`: its search text appears more than once.
    Ambiguous { index: usize },
    /// The edits leave the file exactly as it was.
    NoChange,
}

impl EditError {
    pub fn message(&self) -> Message {
        match self {
            EditError::Unreadable => Message::new("edit-unreadable"),
            EditError::NotFound { index } => {
                Message::new("edit-not-found").num("index", *index)
            }
            EditError::Ambiguous { index } => {
                Message::new("edit-ambiguous").num("index", *index)
            }
            EditError::NoChange => Message::new("edit-no-change"),
        }
    }
}

/// Reads the model's reply. Text outside the blocks (code fences, chatter) is ignored.
pub fn parse_output(reply: &str) -> Result<Output, EditError> {
    enum State {
        Outside,
        InSearch,
        InReplace,
    }
    let mut state = State::Outside;
    let mut edits = Vec::new();
    let mut search: Vec<&str> = Vec::new();
    let mut replace: Vec<&str> = Vec::new();
    let mut missing: Option<String> = None;

    for line in reply.lines() {
        let marker = line.trim();
        match state {
            State::Outside => {
                if marker == SEARCH {
                    search.clear();
                    replace.clear();
                    state = State::InSearch;
                } else if missing.is_none() {
                    if let Some(detail) = marker.strip_prefix(MISSING) {
                        missing = Some(detail.trim().to_string());
                    }
                }
            }
            // A marker in the wrong place means the block is malformed. Guessing
            // what was meant would put marker lines into the user's file.
            State::InSearch => {
                if marker == DIVIDER {
                    state = State::InReplace;
                } else if marker == SEARCH || marker == REPLACE {
                    return Err(EditError::Unreadable);
                } else {
                    search.push(line);
                }
            }
            State::InReplace => {
                if marker == REPLACE {
                    edits.push(Edit { search: search.join("\n"), replace: replace.join("\n") });
                    state = State::Outside;
                } else if marker == SEARCH || marker == DIVIDER {
                    return Err(EditError::Unreadable);
                } else {
                    replace.push(line);
                }
            }
        }
    }

    // A block left open means the reply was cut off: do not apply half of it.
    if !matches!(state, State::Outside) {
        return Err(EditError::Unreadable);
    }
    if !edits.is_empty() {
        return Ok(Output::Edits(edits));
    }
    missing.map(Output::Missing).ok_or(EditError::Unreadable)
}

/// Reads the reply to a new-file card: the whole file inside one code fence.
/// It becomes a single edit with an empty search, which fills an empty file.
pub fn parse_new_file(reply: &str) -> Result<Output, EditError> {
    let mut content: Vec<&str> = Vec::new();
    let mut fences = 0;
    let mut missing: Option<String> = None;
    for line in reply.lines() {
        if line.trim_start().starts_with("```") {
            fences += 1;
        } else if fences == 1 {
            content.push(line);
        } else if fences == 0 && missing.is_none() {
            if let Some(detail) = line.trim().strip_prefix(MISSING) {
                missing = Some(detail.trim().to_string());
            }
        }
    }
    match fences {
        // Exactly one opened and closed block: anything else is ambiguous or cut off.
        2 if !content.join("").trim().is_empty() => {
            Ok(Output::Edits(vec![Edit { search: String::new(), replace: content.join("\n") }]))
        }
        0 => missing.map(Output::Missing).ok_or(EditError::Unreadable),
        _ => Err(EditError::Unreadable),
    }
}

/// Applies the edits in order and returns the new content.
/// The input is never modified, so a failure leaves nothing behind.
pub fn apply(content: &str, edits: &[Edit]) -> Result<String, EditError> {
    let mut result = content.to_string();
    for (i, edit) in edits.iter().enumerate() {
        let index = i + 1;
        if edit.search.trim().is_empty() {
            // An empty search is only meaningful for filling an empty file.
            if !result.trim().is_empty() {
                return Err(EditError::NotFound { index });
            }
            result = format!("{}\n", edit.replace);
            continue;
        }
        match result.matches(&edit.search).count() {
            0 => return Err(EditError::NotFound { index }),
            1 => result = result.replacen(&edit.search, &edit.replace, 1),
            _ => return Err(EditError::Ambiguous { index }),
        }
    }
    if result == content {
        return Err(EditError::NoChange);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(search: &str, replace: &str) -> String {
        format!("{SEARCH}\n{search}\n{DIVIDER}\n{replace}\n{REPLACE}\n")
    }

    fn edits(reply: &str) -> Vec<Edit> {
        match parse_output(reply).unwrap() {
            Output::Edits(e) => e,
            other => panic!("expected edits, got {other:?}"),
        }
    }

    #[test]
    fn parses_blocks_and_ignores_fences_and_chatter() {
        let reply = format!("Sure!\n```\n{}```\n{}", block("a", "b"), block("c\nd", "e"));
        let e = edits(&reply);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1], Edit { search: "c\nd".into(), replace: "e".into() });
    }

    #[test]
    fn parses_the_stop_instruction() {
        assert_eq!(
            parse_output("MISSING: the name of the function"),
            Ok(Output::Missing("the name of the function".into()))
        );
    }

    #[test]
    fn rejects_chatter_and_cut_off_blocks() {
        assert_eq!(parse_output("Done!"), Err(EditError::Unreadable));
        assert_eq!(parse_output(&format!("{SEARCH}\na\n{DIVIDER}\nb")), Err(EditError::Unreadable));
    }

    #[test]
    fn rejects_a_block_with_a_repeated_marker() {
        let doubled = format!("{SEARCH}\na\n{DIVIDER}\nb\n{DIVIDER}\nc\n{REPLACE}\n");
        assert_eq!(parse_output(&doubled), Err(EditError::Unreadable));
        let nested = format!("{SEARCH}\na\n{SEARCH}\nb\n{DIVIDER}\nc\n{REPLACE}\n");
        assert_eq!(parse_output(&nested), Err(EditError::Unreadable));
    }

    #[test]
    fn applies_edits_in_order_keeping_the_rest() {
        let content = "fn a() {}\n\nfn b() {}\n";
        let e = edits(&format!("{}{}", block("fn a() {}", "fn a() { 1; }"), block("fn b() {}", "fn c() {}")));
        assert_eq!(apply(content, &e).unwrap(), "fn a() { 1; }\n\nfn c() {}\n");
    }

    #[test]
    fn refuses_missing_ambiguous_and_unchanged() {
        let content = "x\nx\ny\n";
        let one = |s: &str, r: &str| vec![Edit { search: s.into(), replace: r.into() }];
        assert_eq!(apply(content, &one("z", "q")), Err(EditError::NotFound { index: 1 }));
        assert_eq!(apply(content, &one("x", "q")), Err(EditError::Ambiguous { index: 1 }));
        assert_eq!(apply(content, &one("y", "y")), Err(EditError::NoChange));
    }

    #[test]
    fn new_file_reply_is_one_fenced_block() {
        let reply = "Here it is:\n```rust\npub fn a() {}\n\npub fn b() {}\n```\n";
        let e = match parse_new_file(reply).unwrap() {
            Output::Edits(e) => e,
            other => panic!("{other:?}"),
        };
        assert_eq!(apply("", &e).unwrap(), "pub fn a() {}\n\npub fn b() {}\n");
        assert_eq!(parse_new_file("MISSING: the title"), Ok(Output::Missing("the title".into())));
        for bad in ["pub fn a() {}", "```\ncut off", "```\na\n```\n```\nb\n```", "```\n\n```"] {
            assert_eq!(parse_new_file(bad), Err(EditError::Unreadable), "{bad}");
        }
    }

    #[test]
    fn empty_search_fills_only_an_empty_file() {
        let fill = vec![Edit { search: String::new(), replace: "hello".into() }];
        assert_eq!(apply("", &fill).unwrap(), "hello\n");
        assert_eq!(apply("taken\n", &fill), Err(EditError::NotFound { index: 1 }));
    }
}
