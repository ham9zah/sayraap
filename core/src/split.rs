//! Splitting a rejected task into two smaller ones: the prompt that asks for
//! it and the code that reads the answer.
//!
//! ```text
//! TASK 1
//! GOAL: what the first task changes
//! CHECK: a yes-or-no statement that is true when it is done
//! TASK 2
//! GOAL: ...
//! CHECK: ...
//! ```

use crate::task::TaskCard;
use crate::Message;

const TASK: &str = "TASK";
const GOAL: &str = "GOAL:";
const CHECK: &str = "CHECK:";

/// The prompt asking a model to split `card`, as a message key plus arguments.
pub fn split_prompt(card: &TaskCard, current: &str, reason: &str) -> Message {
    Message::new("split-prompt")
        .arg("goal", card.goal.trim())
        .arg("file", &card.file)
        .arg("current", current)
        .arg("reason", reason)
}

/// Reads the two smaller tasks. They inherit the parent's file and its
/// forbidden list, so a split can never widen the scope.
/// Anything but exactly two complete tasks is refused.
pub fn parse_split(reply: &str, parent: &TaskCard) -> Option<[TaskCard; 2]> {
    let mut tasks: Vec<(String, Vec<String>)> = Vec::new();
    for line in reply.lines() {
        let line = line.trim();
        if line.starts_with(TASK) {
            tasks.push((String::new(), Vec::new()));
        } else if let Some(goal) = line.strip_prefix(GOAL) {
            let task = tasks.last_mut()?;
            if !task.0.is_empty() {
                return None;
            }
            task.0 = goal.trim().to_string();
        } else if let Some(check) = line.strip_prefix(CHECK) {
            let check = check.trim();
            if !check.is_empty() {
                tasks.last_mut()?.1.push(check.to_string());
            }
        }
    }
    if tasks.len() != 2 || tasks.iter().any(|(goal, checks)| goal.is_empty() || checks.is_empty()) {
        return None;
    }
    let mut cards = tasks.into_iter().map(|(goal, acceptance)| TaskCard {
        goal,
        file: parent.file.clone(),
        forbidden: parent.forbidden.clone(),
        acceptance,
    });
    Some([cards.next()?, cards.next()?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> TaskCard {
        TaskCard::from_toml("goal = \"big\"\nfile = \"a.rs\"\nforbidden = [\"no x\"]\nacceptance = [\"done\"]\n").unwrap()
    }

    #[test]
    fn reads_two_tasks_that_inherit_file_and_forbidden() {
        let reply = "Sure.\nTASK 1\nGOAL: add the struct\nCHECK: the struct exists\nTASK 2\nGOAL: add the method\nCHECK: the method exists\nCHECK: it is public\n";
        let [first, second] = parse_split(reply, &parent()).unwrap();
        assert_eq!((first.goal.as_str(), first.file.as_str()), ("add the struct", "a.rs"));
        assert_eq!(first.forbidden, ["no x"]);
        assert_eq!(second.acceptance, ["the method exists", "it is public"]);
    }

    #[test]
    fn refuses_anything_but_two_complete_tasks() {
        let one = "TASK 1\nGOAL: a\nCHECK: b\n";
        let three = "TASK 1\nGOAL: a\nCHECK: b\nTASK 2\nGOAL: a\nCHECK: b\nTASK 3\nGOAL: a\nCHECK: b\n";
        let no_check = "TASK 1\nGOAL: a\nCHECK: b\nTASK 2\nGOAL: a\n";
        let stray = "GOAL: a\nCHECK: b\nTASK 1\nGOAL: a\nCHECK: b\n";
        for bad in [one, three, no_check, stray, "Done!"] {
            assert!(parse_split(bad, &parent()).is_none(), "{bad}");
        }
    }
}
