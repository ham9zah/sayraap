//! One task from card to verdict: ask the model, apply its edits, check
//! scope and size, then commit — or leave the project exactly as it was.

use crate::edit::{apply, parse_output, EditError, Output};
use crate::limits::{changed_lines, Limits};
use crate::llm::{chat, Endpoint, LlmError};
use crate::repo::{Repo, RepoError};
use crate::task::TaskCard;
use crate::Message;
use std::io::ErrorKind;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// The commit that holds this step.
    pub commit: String,
    pub changed_lines: usize,
}

/// Why a task was not accepted. The project is untouched in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    Llm(LlmError),
    Edit(EditError),
    /// The model said what it lacks instead of guessing.
    MissingInfo { detail: String },
    TooLarge { changed: usize, limit: usize },
    /// Something other than the card's file changed.
    OutOfScope { files: Vec<String> },
}

impl Rejection {
    pub fn message(&self) -> Message {
        match self {
            Rejection::Llm(e) => e.message(),
            Rejection::Edit(e) => e.message(),
            Rejection::MissingInfo { detail } => {
                Message::new("task-missing-info").arg("detail", detail)
            }
            Rejection::TooLarge { changed, limit } => {
                Message::new("task-too-large").num("changed", *changed).num("limit", *limit)
            }
            Rejection::OutOfScope { files } => {
                Message::new("task-out-of-scope").arg("files", files.join(", "))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Accepted(Accepted),
    Rejected(Rejection),
}

/// What happened to a task, with the tokens it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub verdict: Verdict,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// The task could not even start, or the project could not be saved or restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    Repo(RepoError),
    Unreadable { path: String },
    Unwritable { path: String },
}

impl RunError {
    pub fn message(&self) -> Message {
        match self {
            RunError::Repo(e) => e.message(),
            RunError::Unreadable { path } => Message::new("file-unreadable").arg("path", path),
            RunError::Unwritable { path } => Message::new("file-unwritable").arg("path", path),
        }
    }
}

impl From<RepoError> for RunError {
    fn from(e: RepoError) -> Self {
        RunError::Repo(e)
    }
}

/// Current content of the card's file. A file that does not exist yet is
/// empty, so a task can create it.
pub fn read_target(project: &Path, card: &TaskCard) -> Result<String, RunError> {
    let path = project.join(&card.file);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err(RunError::Unreadable { path: path.display().to_string() }),
    }
}

/// Runs one task. `render` turns the card's prompt message into text in the
/// project's language; the core itself holds no text.
pub fn run_task(
    project: &Path,
    card: &TaskCard,
    endpoint: &Endpoint,
    limits: Limits,
    render: &dyn Fn(&Message) -> String,
) -> Result<Report, RunError> {
    // Check the project before spending any tokens.
    let repo = Repo::open(project)?;
    repo.require_clean()?;
    let current = read_target(project, card)?;

    let reply = match chat(endpoint, &render(&card.prompt(&current))) {
        Ok(reply) => reply,
        Err(e) => {
            return Ok(Report {
                verdict: Verdict::Rejected(Rejection::Llm(e)),
                prompt_tokens: 0,
                completion_tokens: 0,
            })
        }
    };
    let verdict = judge_and_save(&repo, project, card, &current, &reply.text, limits)?;
    Ok(Report {
        verdict,
        prompt_tokens: reply.prompt_tokens,
        completion_tokens: reply.completion_tokens,
    })
}

/// Decides on the model's reply. Everything that can be checked in memory is
/// checked before the file is written; after writing, any failure rolls back.
fn judge_and_save(
    repo: &Repo,
    project: &Path,
    card: &TaskCard,
    current: &str,
    reply: &str,
    limits: Limits,
) -> Result<Verdict, RunError> {
    let reject = |r: Rejection| Ok(Verdict::Rejected(r));

    let edits = match parse_output(reply) {
        Ok(Output::Edits(edits)) => edits,
        Ok(Output::Missing(detail)) => return reject(Rejection::MissingInfo { detail }),
        Err(e) => return reject(Rejection::Edit(e)),
    };
    let updated = match apply(current, &edits) {
        Ok(updated) => updated,
        Err(e) => return reject(Rejection::Edit(e)),
    };
    let changed = changed_lines(current, &updated);
    if changed > limits.max_changed_lines {
        return reject(Rejection::TooLarge { changed, limit: limits.max_changed_lines });
    }

    let path = project.join(&card.file);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, updated).is_err() {
        repo.rollback()?;
        return Err(RunError::Unwritable { path: path.display().to_string() });
    }

    let files = repo.changed_files()?;
    if files != [card.file.as_str()] {
        repo.rollback()?;
        return reject(Rejection::OutOfScope { files });
    }
    match repo.commit_file(&card.file, card.goal.trim()) {
        Ok(commit) => Ok(Verdict::Accepted(Accepted { commit, changed_lines: changed })),
        Err(e) => {
            repo.rollback()?;
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testing::project;

    const START: &str = "pub fn greet() {}\n";

    fn card() -> TaskCard {
        TaskCard::from_toml("goal = \"Add farewell\"\nfile = \"greet.rs\"\nacceptance = [\"farewell exists\"]\n")
            .unwrap()
    }

    fn reply(replacement: &str) -> String {
        format!("<<<<<<< SEARCH\npub fn greet() {{}}\n=======\n{replacement}\n>>>>>>> REPLACE\n")
    }

    fn judge(text: &str, limit: usize) -> (Verdict, String, bool) {
        let (dir, repo) = project(START);
        let limits = Limits { max_changed_lines: limit };
        let verdict = judge_and_save(&repo, dir.path(), &card(), START, text, limits).unwrap();
        let content = std::fs::read_to_string(dir.path().join("greet.rs")).unwrap();
        (verdict, content, repo.changed_files().unwrap().is_empty())
    }

    #[test]
    fn a_good_reply_becomes_one_commit() {
        let (verdict, content, clean) = judge(&reply("pub fn greet() {}\npub fn farewell() {}"), 30);
        match verdict {
            Verdict::Accepted(a) => assert_eq!((a.commit.len(), a.changed_lines), (40, 1)),
            other => panic!("{other:?}"),
        }
        assert!(content.contains("farewell"));
        assert!(clean);
    }

    #[test]
    fn rejections_leave_no_trace() {
        let big = reply("pub fn greet() {}\n// 1\n// 2\n// 3");
        let cases: [(&str, usize); 4] = [("Done!", 30), ("MISSING: the name", 30), (&big, 2), (&reply("pub fn greet() {}"), 30)];
        for (text, limit) in cases {
            let (verdict, content, clean) = judge(text, limit);
            assert!(matches!(verdict, Verdict::Rejected(_)), "{text}");
            assert_eq!(content, START, "{text}");
            assert!(clean, "{text}");
        }
        let (verdict, _, _) = judge(&big, 2);
        assert_eq!(verdict, Verdict::Rejected(Rejection::TooLarge { changed: 3, limit: 2 }));
    }

    #[test]
    fn a_stray_file_is_out_of_scope_and_rolled_back() {
        let (dir, repo) = project(START);
        // Stands in for a tool touching another file while the task runs.
        std::fs::write(dir.path().join("other.rs"), "x").unwrap();
        let text = reply("pub fn greet() {}\npub fn farewell() {}");
        let verdict = judge_and_save(&repo, dir.path(), &card(), START, &text, Limits::default()).unwrap();
        assert!(matches!(verdict, Verdict::Rejected(Rejection::OutOfScope { .. })));
        assert!(!dir.path().join("other.rs").exists());
        assert_eq!(std::fs::read_to_string(dir.path().join("greet.rs")).unwrap(), START);
    }

    #[test]
    fn a_dirty_project_is_refused_before_calling_the_model() {
        let (dir, _repo) = project(START);
        std::fs::write(dir.path().join("greet.rs"), "user's unsaved work\n").unwrap();
        let endpoint = Endpoint { base_url: "http://127.0.0.1:9/v1".into(), model: "x".into(), timeout: std::time::Duration::from_secs(1) };
        let result = run_task(dir.path(), &card(), &endpoint, Limits::default(), &|_| String::new());
        assert!(matches!(result, Err(RunError::Repo(RepoError::Dirty { .. }))));
        assert_eq!(std::fs::read_to_string(dir.path().join("greet.rs")).unwrap(), "user's unsaved work\n");
    }
}
