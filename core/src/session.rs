//! Runs a task and writes down what happened: the task, each attempt, the
//! model's raw reply, the reason for a rejection and the commit of an acceptance.

use crate::check::Check;
use crate::limits::Limits;
use crate::runner::{preflight, run_task, Ask, Report, RunError, Verdict};
use crate::store::{AttemptEnd, AttemptOutcome, Store, StoreError, TaskStatus};
use crate::task::TaskCard;
use crate::Message;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub task: i64,
    /// Which attempt of the task this was, counting from 1.
    pub attempt: i64,
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Store(StoreError),
    Run(RunError),
}

impl SessionError {
    pub fn message(&self) -> Message {
        match self {
            SessionError::Store(e) => e.message(),
            SessionError::Run(e) => e.message(),
        }
    }
}

impl From<StoreError> for SessionError {
    fn from(e: StoreError) -> Self {
        SessionError::Store(e)
    }
}

/// Everything a run needs besides the card.
pub struct RunSetup<'a> {
    pub project: &'a Path,
    pub model: &'a str,
    pub ask: Ask<'a>,
    pub limits: Limits,
    pub check: Option<&'a Check>,
    pub render: &'a dyn Fn(&Message) -> String,
}

/// Records a new task from the card and runs its first attempt.
pub fn run_recorded(
    store: &Store,
    card: &TaskCard,
    setup: &RunSetup,
) -> Result<Recorded, SessionError> {
    // A project that cannot be worked on gets no record: nothing was attempted.
    preflight(setup.project).map_err(SessionError::Run)?;

    let project = store.project(setup.project)?;
    let task = store.add_task(project, card)?;
    store.set_status(task, TaskStatus::Running, None)?;
    let (attempt_id, attempt) = store.start_attempt(task, setup.model)?;

    let result = run_task(setup.project, card, setup.ask, setup.limits, setup.check, setup.render);
    match result {
        Ok(report) => {
            let (outcome, status, commit, reason) = match &report.verdict {
                Verdict::Accepted(a) => {
                    (AttemptOutcome::Accepted, TaskStatus::Accepted, Some(a.commit.as_str()), None)
                }
                Verdict::Rejected(r) => {
                    (AttemptOutcome::Rejected, TaskStatus::Rejected, None, Some(r.message()))
                }
            };
            store.finish_attempt(attempt_id, &AttemptEnd {
                outcome,
                reason: reason.as_ref(),
                reply: report.reply.as_deref(),
                prompt_tokens: report.prompt_tokens,
                completion_tokens: report.completion_tokens,
            })?;
            store.set_status(task, status, commit)?;
            Ok(Recorded { task, attempt, report })
        }
        Err(error) => {
            // The attempt could not be judged; the task goes back to waiting.
            store.finish_attempt(attempt_id, &AttemptEnd {
                outcome: AttemptOutcome::Error,
                reason: Some(&error.message()),
                reply: None,
                prompt_tokens: 0,
                completion_tokens: 0,
            })?;
            store.set_status(task, TaskStatus::Pending, None)?;
            Err(SessionError::Run(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmError, Reply};
    use crate::repo::testing::project;
    use crate::repo::RepoError;

    const START: &str = "pub fn greet() {}\n";

    fn card() -> TaskCard {
        TaskCard::from_toml("goal = \"Add farewell\"\nfile = \"greet.rs\"\nacceptance = [\"farewell exists\"]\n")
            .unwrap()
    }

    fn run(store: &Store, dir: &Path, answer: Result<&str, LlmError>) -> Result<Recorded, SessionError> {
        let ask = |_: &str| {
            answer.clone().map(|text| Reply { text: text.to_string(), prompt_tokens: 100, completion_tokens: 20 })
        };
        let render = |_: &Message| String::new();
        let setup = RunSetup { project: dir, model: "fake", ask: &ask, limits: Limits::default(), check: None, render: &render };
        run_recorded(store, &card(), &setup)
    }

    #[test]
    fn an_accepted_task_is_recorded_with_its_commit() {
        let (dir, _repo) = project(START);
        let store = Store::open_in_memory().unwrap();
        let reply = "<<<<<<< SEARCH\npub fn greet() {}\n=======\npub fn greet() {}\npub fn farewell() {}\n>>>>>>> REPLACE\n";
        let recorded = run(&store, dir.path(), Ok(reply)).unwrap();
        assert_eq!(recorded.attempt, 1);

        let tasks = store.tasks(store.project(dir.path()).unwrap()).unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Accepted);
        assert_eq!(tasks[0].commit.as_ref().map(String::len), Some(40));
        let attempts = store.attempts(recorded.task).unwrap();
        assert_eq!(attempts[0].outcome, AttemptOutcome::Accepted);
        assert_eq!(attempts[0].reply.as_deref(), Some(reply));
        assert_eq!((attempts[0].prompt_tokens, attempts[0].completion_tokens), (100, 20));
    }

    #[test]
    fn a_rejected_task_keeps_the_reply_and_the_reason() {
        let (dir, _repo) = project(START);
        let store = Store::open_in_memory().unwrap();
        let recorded = run(&store, dir.path(), Ok("Done!")).unwrap();
        let attempts = store.attempts(recorded.task).unwrap();
        assert_eq!(attempts[0].outcome, AttemptOutcome::Rejected);
        assert_eq!(attempts[0].reason.as_ref().map(|m| m.key.as_str()), Some("edit-unreadable"));
        assert_eq!(attempts[0].reply.as_deref(), Some("Done!"));
        let tasks = store.tasks(store.project(dir.path()).unwrap()).unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Rejected);
    }

    #[test]
    fn an_unreachable_model_is_a_recorded_rejection() {
        let (dir, _repo) = project(START);
        let store = Store::open_in_memory().unwrap();
        let recorded = run(&store, dir.path(), Err(LlmError::Unreachable { url: "u".into() })).unwrap();
        let attempts = store.attempts(recorded.task).unwrap();
        assert_eq!(attempts[0].reason.as_ref().map(|m| m.key.as_str()), Some("llm-unreachable"));
        assert_eq!(attempts[0].reply, None);
    }

    #[test]
    fn a_dirty_project_records_nothing() {
        let (dir, _repo) = project(START);
        std::fs::write(dir.path().join("greet.rs"), "unsaved\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        let result = run(&store, dir.path(), Ok("unused"));
        assert!(matches!(result, Err(SessionError::Run(RunError::Repo(RepoError::Dirty { .. })))));
        assert!(store.tasks(store.project(dir.path()).unwrap()).unwrap().is_empty());
    }
}
