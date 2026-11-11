//! One task from card to verdict: ask the model, apply its edits, check
//! scope and size, then commit — or leave the project exactly as it was.

use crate::check::{run_check, Check, CheckResult};
use crate::edit::{apply, parse_new_file, parse_output, EditError, Output};
use crate::limits::{changed_lines, Limits};
use crate::llm::{LlmError, Reply};
use crate::repo::{Repo, RepoError};
use crate::task::TaskCard;
use crate::Message;
use std::io::ErrorKind;
use std::path::Path;

/// The message shown when a task is accepted.
pub fn accepted_message(card: &TaskCard, accepted: &Accepted) -> Message {
    Message::new("task-accepted")
        .arg("file", &card.file)
        .num("lines", accepted.changed_lines)
        .arg("commit", &accepted.commit[..accepted.commit.len().min(7)])
}

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
    /// The project's check command reported a problem with the change.
    CheckFailed { detail: String },
    CheckTimedOut { seconds: u64 },
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
            Rejection::CheckFailed { detail } => {
                Message::new("task-check-failed").arg("detail", detail)
            }
            Rejection::CheckTimedOut { seconds } => {
                Message::new("task-check-timeout").num("seconds", *seconds)
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
    /// The model's raw reply, if it answered at all.
    pub reply: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// The task could not even start, or the project could not be saved or restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    Repo(RepoError),
    Unreadable { path: String },
    Unwritable { path: String },
    /// The check command could not be started, so nothing can be judged.
    CheckUnavailable { command: String },
}

impl RunError {
    pub fn message(&self) -> Message {
        match self {
            RunError::Repo(e) => e.message(),
            RunError::Unreadable { path } => Message::new("file-unreadable").arg("path", path),
            RunError::Unwritable { path } => Message::new("file-unwritable").arg("path", path),
            RunError::CheckUnavailable { command } => {
                Message::new("check-unavailable").arg("command", command)
            }
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

/// How the runner reaches a model: prompt in, reply out. Local and external
/// providers, and tests, all fit behind this.
pub type Ask<'a> = &'a dyn Fn(&str) -> Result<Reply, LlmError>;

/// Checks that the project can be worked on and rolled back. Costs no tokens.
pub fn preflight(project: &Path) -> Result<Repo, RunError> {
    let repo = Repo::open(project)?;
    repo.require_clean()?;
    Ok(repo)
}

/// Runs one attempt of a task. `render` turns the card's prompt message into text in the
/// project's language; the core itself holds no text.
pub fn run_task(
    project: &Path,
    card: &TaskCard,
    ask: Ask,
    limits: Limits,
    check: Option<&Check>,
    render: &dyn Fn(&Message) -> String,
    note: Option<&str>,
) -> Result<Report, RunError> {
    // Check the project before spending any tokens.
    let repo = preflight(project)?;
    let current = read_target(project, card)?;

    // `note` carries the reason a previous attempt was rejected.
    let mut prompt = render(&card.prompt(&current));
    if let Some(note) = note {
        prompt.push_str("\n\n");
        prompt.push_str(note);
    }
    let reply = match ask(&prompt) {
        Ok(reply) => reply,
        Err(e) => {
            return Ok(Report {
                verdict: Verdict::Rejected(Rejection::Llm(e)),
                reply: None,
                prompt_tokens: 0,
                completion_tokens: 0,
            })
        }
    };
    let verdict = judge_and_save(&repo, project, card, &current, &reply.text, limits, check)?;
    Ok(Report {
        verdict,
        reply: Some(reply.text),
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
    check: Option<&Check>,
) -> Result<Verdict, RunError> {
    let reject = |r: Rejection| Ok(Verdict::Rejected(r));

    let parsed =
        if TaskCard::is_new_file(current) { parse_new_file(reply) } else { parse_output(reply) };
    let edits = match parsed {
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

    if let Some(check) = check {
        match run_check(project, check) {
            CheckResult::Passed => {}
            CheckResult::Failed { detail } => {
                repo.rollback()?;
                return reject(Rejection::CheckFailed { detail });
            }
            CheckResult::TimedOut => {
                repo.rollback()?;
                return reject(Rejection::CheckTimedOut { seconds: check.timeout.as_secs() });
            }
            CheckResult::Unavailable => {
                repo.rollback()?;
                return Err(RunError::CheckUnavailable { command: check.command.join(" ") });
            }
        }
    }

    // Looked at after the check, so files a check leaves behind count too.
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
        let verdict = judge_and_save(&repo, dir.path(), &card(), START, text, limits, None).unwrap();
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
        let verdict = judge_and_save(&repo, dir.path(), &card(), START, &text, Limits::default(), None).unwrap();
        assert!(matches!(verdict, Verdict::Rejected(Rejection::OutOfScope { .. })));
        assert!(!dir.path().join("other.rs").exists());
        assert_eq!(std::fs::read_to_string(dir.path().join("greet.rs")).unwrap(), START);
    }

    #[cfg(unix)]
    #[test]
    fn the_check_command_is_the_judge() {
        use std::time::Duration;
        let text = reply("pub fn greet() {}\npub fn farewell() {}");
        let run = |line: &str| {
            let (dir, repo) = project(START);
            let check = Check::from_line(line, Duration::from_secs(5)).unwrap();
            let verdict = judge_and_save(&repo, dir.path(), &card(), START, &text, Limits::default(), Some(&check));
            let content = std::fs::read_to_string(dir.path().join("greet.rs")).unwrap();
            (verdict, content, repo.changed_files().unwrap().is_empty())
        };
        // Passes only if the new text is really in the file when the check runs.
        let (verdict, content, clean) = run("grep -q farewell greet.rs");
        assert!(matches!(verdict, Ok(Verdict::Accepted(_))));
        assert!(content.contains("farewell") && clean);

        let (verdict, content, clean) = run("grep -q no-such-text greet.rs");
        assert!(matches!(verdict, Ok(Verdict::Rejected(Rejection::CheckFailed { .. }))));
        assert!(content == START && clean);

        let (verdict, content, clean) = run("no-such-program-sayraap");
        assert!(matches!(verdict, Err(RunError::CheckUnavailable { .. })));
        assert!(content == START && clean);
    }

    #[test]
    fn a_new_file_is_created_from_a_fenced_reply() {
        let (dir, repo) = project(START);
        let card = TaskCard::from_toml("goal = \"g\"\nfile = \"src/new.rs\"\nacceptance = [\"a\"]\n").unwrap();
        let verdict = judge_and_save(&repo, dir.path(), &card, "", "```rust\npub fn n() {}\n```", Limits::default(), None).unwrap();
        assert!(matches!(verdict, Verdict::Accepted(_)));
        assert_eq!(std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap(), "pub fn n() {}\n");
        assert!(repo.changed_files().unwrap().is_empty());
    }

    #[test]
    fn a_dirty_project_is_refused_before_calling_the_model() {
        let (dir, _repo) = project(START);
        std::fs::write(dir.path().join("greet.rs"), "user's unsaved work\n").unwrap();
        let ask = |_: &str| -> Result<Reply, LlmError> { panic!("the model must not be called") };
        let result = run_task(dir.path(), &card(), &ask, Limits::default(), None, &|_| String::new(), None);
        assert!(matches!(result, Err(RunError::Repo(RepoError::Dirty { .. }))));
        assert_eq!(std::fs::read_to_string(dir.path().join("greet.rs")).unwrap(), "user's unsaved work\n");
    }
}
