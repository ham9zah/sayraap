//! The task card: the fixed-shape description of one small change to one file.

use crate::Message;
use serde::Deserialize;
use std::path::{Component, Path};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCard {
    /// What is wanted, in a sentence or two.
    pub goal: String,
    /// The only file the task may change, relative to the project root.
    pub file: String,
    /// Things the task must not do.
    #[serde(default)]
    pub forbidden: Vec<String>,
    /// Acceptance criteria, each answerable with yes or no.
    pub acceptance: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardError {
    /// Not valid TOML, or a field is missing or unknown.
    Unreadable { detail: String },
    /// `goal` is empty.
    NoGoal,
    /// There is no acceptance criterion to check the result against.
    NoAcceptance,
    /// `file` is absolute or climbs out of the project with `..`.
    BadPath { file: String },
}

impl CardError {
    pub fn message(&self) -> Message {
        match self {
            CardError::Unreadable { detail } => Message::new("card-unreadable").arg("detail", detail),
            CardError::NoGoal => Message::new("card-no-goal"),
            CardError::NoAcceptance => Message::new("card-no-acceptance"),
            CardError::BadPath { file } => Message::new("card-bad-path").arg("file", file),
        }
    }
}

impl TaskCard {
    pub fn from_toml(source: &str) -> Result<Self, CardError> {
        let card: TaskCard = toml::from_str(source)
            .map_err(|e| CardError::Unreadable { detail: e.message().to_string() })?;
        if card.goal.trim().is_empty() {
            return Err(CardError::NoGoal);
        }
        if card.acceptance.iter().all(|a| a.trim().is_empty()) {
            return Err(CardError::NoAcceptance);
        }
        let inside_project = !card.file.is_empty()
            && Path::new(&card.file).components().all(|c| matches!(c, Component::Normal(_)));
        if !inside_project {
            return Err(CardError::BadPath { file: card.file });
        }
        Ok(card)
    }

    /// The prompt sent to the executor, as a message key plus arguments.
    /// `current` is the part of the file shown to the model. An empty file
    /// gets its own card, which asks for the whole content instead of edits.
    pub fn prompt(&self, current: &str) -> Message {
        let message = if Self::is_new_file(current) {
            Message::new("card-prompt-new")
        } else {
            Message::new("card-prompt").arg("current", current)
        };
        message
            .arg("goal", self.goal.trim())
            .arg("file", &self.file)
            .arg("forbidden", bullet_list(&self.forbidden))
            .arg("acceptance", bullet_list(&self.acceptance))
    }

    /// True when the task creates the file rather than editing it.
    pub fn is_new_file(current: &str) -> bool {
        current.trim().is_empty()
    }
}

fn bullet_list(items: &[String]) -> String {
    let lines: Vec<String> = items
        .iter()
        .filter(|i| !i.trim().is_empty())
        .map(|i| format!("- {}", i.trim()))
        .collect();
    if lines.is_empty() { "-".to_string() } else { lines.join("\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
goal = "Add a farewell function."
file = "src/greet.rs"
forbidden = ["Do not change greet"]
acceptance = ["A function named farewell exists"]
"#;

    #[test]
    fn reads_a_good_card() {
        let card = TaskCard::from_toml(GOOD).unwrap();
        assert_eq!(card.file, "src/greet.rs");
        let prompt = card.prompt("fn greet() {}");
        assert_eq!(prompt.key, "card-prompt");
        assert_eq!(prompt.text_arg("forbidden"), Some("- Do not change greet"));
    }

    #[test]
    fn an_empty_file_gets_the_new_file_card() {
        let card = TaskCard::from_toml(GOOD).unwrap();
        assert_eq!(card.prompt("  \n").key, "card-prompt-new");
        assert_eq!(card.prompt("  \n").text_arg("current"), None);
    }

    #[test]
    fn rejects_paths_outside_the_project() {
        for file in ["../secret.txt", "/etc/passwd", ""] {
            let source = GOOD.replace("src/greet.rs", file);
            assert!(matches!(TaskCard::from_toml(&source), Err(CardError::BadPath { .. })), "{file}");
        }
    }

    #[test]
    fn rejects_a_card_without_acceptance() {
        let source = GOOD.replace(r#"["A function named farewell exists"]"#, "[]");
        assert_eq!(TaskCard::from_toml(&source), Err(CardError::NoAcceptance));
    }

    #[test]
    fn rejects_unknown_fields() {
        let source = format!("{GOOD}\nextra = 1\n");
        assert!(matches!(TaskCard::from_toml(&source), Err(CardError::Unreadable { .. })));
    }
}
