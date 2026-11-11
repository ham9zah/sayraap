//! Runs a task through the escalation ladder and writes down everything that
//! happens. The ladder only goes one way and every rung is bounded, so a task
//! always ends as accepted or stuck — it can never loop.
//!
//! 1. A second attempt, with the reason for the rejection attached.
//! 2. One split into two smaller tasks, which cannot be split again.
//! 3. (An external model, once — added in a later phase.)
//! 4. Stuck: the task waits for the user, with all its attempts on record.

use crate::check::Check;
use crate::limits::Limits;
use crate::llm::LlmError;
use crate::runner::{
    accepted_message, preflight, read_target, run_task, Ask, Rejection, RunError, Verdict,
};
use crate::split::{parse_split, split_prompt};
use crate::store::{
    AttemptEnd, AttemptKind, AttemptOutcome, Detector, Store, StoreError, TaskStatus,
};
use crate::task::TaskCard;
use crate::Message;
use std::path::Path;

/// How many times a task is tried as written before the ladder moves on.
pub const MAX_ATTEMPTS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LadderResult {
    pub task: i64,
    /// `Accepted` or `Stuck`.
    pub status: TaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Store(StoreError),
    Run(RunError),
    /// The model server cannot be reached. Not the task's fault: it goes back to waiting.
    ModelDown,
}

impl SessionError {
    pub fn message(&self) -> Message {
        match self {
            SessionError::Store(e) => e.message(),
            SessionError::Run(e) => e.message(),
            SessionError::ModelDown => Message::new("ladder-model-down"),
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
    /// Turns a message into text in the project's language, for prompts.
    pub render: &'a dyn Fn(&Message) -> String,
    /// Receives progress as it happens, for the front-end to show.
    pub events: &'a dyn Fn(&Message),
}

/// Records a new task from the card and runs it to its end.
pub fn run_ladder(
    store: &Store,
    card: &TaskCard,
    setup: &RunSetup,
) -> Result<LadderResult, SessionError> {
    // A project that cannot be worked on gets no record: nothing was attempted.
    preflight(setup.project).map_err(SessionError::Run)?;
    let project = store.project(setup.project)?;
    let task = store.add_task(project, card, None)?;
    let status = work(store, project, task, card, setup, true)?;
    Ok(LadderResult { task, status })
}

/// Which detector, if any, explains this rejection. Repetition is looked for
/// first because it is the one that changes what the ladder does next.
fn detect(
    rejection: &Rejection,
    reply: Option<&str>,
    earlier_replies: &[String],
    reason: &Message,
    previous_reason: Option<&Message>,
) -> Option<Detector> {
    if reply.is_some_and(|r| earlier_replies.iter().any(|e| e == r.trim())) {
        return Some(Detector::RepeatOutput);
    }
    if previous_reason == Some(reason) {
        return Some(Detector::RepeatError);
    }
    match rejection {
        Rejection::Edit(crate::edit::EditError::NoChange) => Some(Detector::NoChange),
        Rejection::TooLarge { .. } | Rejection::OutOfScope { .. } => Some(Detector::OutOfScope),
        Rejection::Llm(LlmError::TimedOut { .. }) | Rejection::CheckTimedOut { .. } => {
            Some(Detector::Timeout)
        }
        _ => None,
    }
}

fn stuck(store: &Store, task: i64, setup: &RunSetup) -> Result<TaskStatus, SessionError> {
    store.set_status(task, TaskStatus::Stuck, None)?;
    (setup.events)(&Message::new("ladder-stuck").num("task", task));
    Ok(TaskStatus::Stuck)
}

/// Takes one task up the ladder. `may_split` is false for tasks that came
/// from a split, which is what keeps the splitting to one level.
fn work(
    store: &Store,
    project: i64,
    task: i64,
    card: &TaskCard,
    setup: &RunSetup,
    may_split: bool,
) -> Result<TaskStatus, SessionError> {
    store.set_status(task, TaskStatus::Running, None)?;
    let mut replies: Vec<String> = Vec::new();
    let mut last_reason: Option<Message> = None;

    for _ in 0..MAX_ATTEMPTS {
        let note = last_reason.as_ref().map(|reason| {
            (setup.render)(&Message::new("card-retry-note").arg("reason", (setup.render)(reason)))
        });
        let (attempt_id, number) = store.start_attempt(task, setup.model, AttemptKind::Execute)?;
        (setup.events)(&Message::new("ladder-attempt").num("task", task).num("number", number));

        let report = match run_task(
            setup.project,
            card,
            setup.ask,
            setup.limits,
            setup.check,
            setup.render,
            note.as_deref(),
        ) {
            Ok(report) => report,
            Err(error) => {
                // The attempt could not be judged; the task goes back to waiting.
                store.finish_attempt(attempt_id, &AttemptEnd {
                    outcome: AttemptOutcome::Error,
                    detector: None,
                    reason: Some(&error.message()),
                    reply: None,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                })?;
                store.set_status(task, TaskStatus::Pending, None)?;
                return Err(SessionError::Run(error));
            }
        };
        if report.prompt_tokens + report.completion_tokens > 0 {
            (setup.events)(
                &Message::new("llm-tokens")
                    .arg("model", setup.model)
                    .num("prompt", report.prompt_tokens)
                    .num("completion", report.completion_tokens),
            );
        }

        let rejection = match &report.verdict {
            Verdict::Accepted(accepted) => {
                store.finish_attempt(attempt_id, &AttemptEnd {
                    outcome: AttemptOutcome::Accepted,
                    detector: None,
                    reason: None,
                    reply: report.reply.as_deref(),
                    prompt_tokens: report.prompt_tokens,
                    completion_tokens: report.completion_tokens,
                })?;
                store.set_status(task, TaskStatus::Accepted, Some(&accepted.commit))?;
                (setup.events)(&accepted_message(card, accepted));
                return Ok(TaskStatus::Accepted);
            }
            Verdict::Rejected(rejection) => rejection,
        };

        let reason = rejection.message();
        let detector =
            detect(rejection, report.reply.as_deref(), &replies, &reason, last_reason.as_ref());
        store.finish_attempt(attempt_id, &AttemptEnd {
            outcome: AttemptOutcome::Rejected,
            detector,
            reason: Some(&reason),
            reply: report.reply.as_deref(),
            prompt_tokens: report.prompt_tokens,
            completion_tokens: report.completion_tokens,
        })?;
        (setup.events)(&Message::new("task-rejected"));
        (setup.events)(&reason);
        if let Some(detector) = detector {
            (setup.events)(&Message::new(format!("detector-{}", detector.as_str())));
        }

        match rejection {
            // No server: trying again or splitting would only fail the same way.
            Rejection::Llm(LlmError::Unreachable { .. } | LlmError::Http { .. }) => {
                store.set_status(task, TaskStatus::Pending, None)?;
                return Err(SessionError::ModelDown);
            }
            // Only the user has the missing fact.
            Rejection::MissingInfo { .. } => return stuck(store, task, setup),
            _ => {}
        }
        if let Some(reply) = &report.reply {
            replies.push(reply.trim().to_string());
        }
        last_reason = Some(reason);
        // Asking the same thing again would give the same answer again.
        if detector == Some(Detector::RepeatOutput) {
            break;
        }
    }

    if may_split {
        let reason = last_reason.as_ref().map(|r| (setup.render)(r)).unwrap_or_default();
        if let Some(children) = split(store, task, card, setup, &reason)? {
            return run_children(store, project, task, children, setup);
        }
    }
    stuck(store, task, setup)
}

/// Asks the model for two smaller tasks. `None` means the split did not work
/// out; there is no second try.
fn split(
    store: &Store,
    task: i64,
    card: &TaskCard,
    setup: &RunSetup,
    reason: &str,
) -> Result<Option<[TaskCard; 2]>, SessionError> {
    (setup.events)(&Message::new("ladder-split").num("task", task));
    let current = read_target(setup.project, card).map_err(SessionError::Run)?;
    let (attempt_id, _) = store.start_attempt(task, setup.model, AttemptKind::Split)?;
    let answer = (setup.ask)(&(setup.render)(&split_prompt(card, &current, reason)));

    let (cards, failure, reply, tokens) = match answer {
        Ok(reply) => {
            let cards = parse_split(&reply.text, card);
            let failure = cards.is_none().then(|| Message::new("split-unreadable"));
            (cards, failure, Some(reply.text), (reply.prompt_tokens, reply.completion_tokens))
        }
        Err(error) => (None, Some(error.message()), None, (0, 0)),
    };
    store.finish_attempt(attempt_id, &AttemptEnd {
        outcome: if cards.is_some() { AttemptOutcome::Accepted } else { AttemptOutcome::Rejected },
        detector: None,
        reason: failure.as_ref(),
        reply: reply.as_deref(),
        prompt_tokens: tokens.0,
        completion_tokens: tokens.1,
    })?;
    if let Some(failure) = &failure {
        (setup.events)(failure);
    }
    Ok(cards)
}

/// Runs the two halves in order. The parent is accepted only if both are;
/// the first half that gets stuck stops the chain.
fn run_children(
    store: &Store,
    project: i64,
    parent: i64,
    children: [TaskCard; 2],
    setup: &RunSetup,
) -> Result<TaskStatus, SessionError> {
    let ids = [
        store.add_task(project, &children[0], Some(parent))?,
        store.add_task(project, &children[1], Some(parent))?,
    ];
    (setup.events)(
        &Message::new("ladder-split-done").num("first", ids[0]).num("second", ids[1]),
    );
    for (index, (id, card)) in ids.iter().zip(&children).enumerate() {
        let status = match work(store, project, *id, card, setup, false) {
            Ok(status) => status,
            Err(error) => {
                store.set_status(parent, TaskStatus::Pending, None)?;
                return Err(error);
            }
        };
        if status != TaskStatus::Accepted {
            for later in &ids[index + 1..] {
                store.set_status(*later, TaskStatus::Cancelled, None)?;
            }
            return stuck(store, parent, setup);
        }
    }
    store.set_status(parent, TaskStatus::Accepted, None)?;
    Ok(TaskStatus::Accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Reply;
    use crate::repo::testing::project;
    use crate::repo::RepoError;
    use crate::store::TaskRow;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    const START: &str = "pub fn greet() {}\n";
    const SPLIT: &str = "TASK 1\nGOAL: add one\nCHECK: one exists\nTASK 2\nGOAL: add two\nCHECK: two exists\n";

    fn card() -> TaskCard {
        TaskCard::from_toml("goal = \"Add farewell\"\nfile = \"greet.rs\"\nacceptance = [\"farewell exists\"]\n")
            .unwrap()
    }

    /// An edit that appends `name` after whatever the file's last function is.
    fn add_after(anchor: &str, name: &str) -> String {
        format!("<<<<<<< SEARCH\npub fn {anchor}() {{}}\n=======\npub fn {anchor}() {{}}\npub fn {name}() {{}}\n>>>>>>> REPLACE\n")
    }

    struct Run {
        result: Result<LadderResult, SessionError>,
        tasks: Vec<TaskRow>,
        prompts: Vec<String>,
        store: Store,
        content: String,
    }

    /// Runs the ladder against scripted answers. Asking for more answers than
    /// were scripted fails the test: that is how the bound on calls is checked.
    fn run(answers: Vec<Result<String, LlmError>>) -> Run {
        let (dir, _repo) = project(START);
        let store = Store::open_in_memory().unwrap();
        let script = RefCell::new(VecDeque::from(answers));
        let prompts = RefCell::new(Vec::new());
        let ask = |prompt: &str| {
            prompts.borrow_mut().push(prompt.to_string());
            let answer = script.borrow_mut().pop_front().expect("the ladder asked the model too many times");
            answer.map(|text| Reply { text, prompt_tokens: 100, completion_tokens: 20 })
        };
        let render = |m: &Message| {
            let args: Vec<&str> = m.args.iter().filter_map(|(n, _)| m.text_arg(n)).collect();
            format!("{} {}", m.key, args.join(" "))
        };
        let events = |_: &Message| {};
        let setup = RunSetup { project: dir.path(), model: "fake", ask: &ask, limits: Limits::default(), check: None, render: &render, events: &events };
        let result = run_ladder(&store, &card(), &setup);
        assert!(script.borrow().is_empty(), "scripted answers left unused");
        let tasks = store.tasks(store.project(dir.path()).unwrap()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("greet.rs")).unwrap();
        Run { result, tasks, prompts: prompts.into_inner(), store, content }
    }

    fn ok(text: &str) -> Result<String, LlmError> {
        Ok(text.to_string())
    }

    #[test]
    fn accepted_first_time_costs_one_call() {
        let r = run(vec![ok(&add_after("greet", "farewell"))]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Accepted);
        assert_eq!(r.tasks[0].commit.as_ref().map(String::len), Some(40));
        assert_eq!(r.store.attempts(r.tasks[0].id).unwrap()[0].reply.as_deref(), Some(add_after("greet", "farewell").as_str()));
    }

    #[test]
    fn the_second_attempt_carries_the_reason_for_the_first_rejection() {
        let r = run(vec![ok("Done!"), ok(&add_after("greet", "farewell"))]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Accepted);
        assert!(!r.prompts[0].contains("card-retry-note"));
        assert!(r.prompts[1].contains("card-retry-note edit-unreadable"));
        let attempts = r.store.attempts(r.tasks[0].id).unwrap();
        assert_eq!((attempts[0].outcome, attempts[1].outcome), (AttemptOutcome::Rejected, AttemptOutcome::Accepted));
    }

    #[test]
    fn an_impossible_task_ends_stuck_within_the_bound() {
        // Two attempts, one failed split: three calls and no more.
        let r = run(vec![ok("Done!"), ok("Done!"), ok("Cannot split.")]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Stuck);
        assert_eq!(r.content, START, "nothing left behind");
        let attempts = r.store.attempts(r.tasks[0].id).unwrap();
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[1].detector, Some(Detector::RepeatOutput));
        assert_eq!(attempts[2].kind, AttemptKind::Split);
        assert_eq!(attempts[2].reason.as_ref().map(|m| m.key.as_str()), Some("split-unreadable"));
    }

    #[test]
    fn a_different_reply_with_the_same_failure_is_a_repeated_error() {
        let r = run(vec![ok("Done!"), ok("All done!"), ok("Cannot split.")]);
        assert_eq!(r.store.attempts(r.tasks[0].id).unwrap()[1].detector, Some(Detector::RepeatError));
    }

    #[test]
    fn a_split_task_is_accepted_when_both_halves_are() {
        let r = run(vec![ok("Done!"), ok("Nope"), ok(SPLIT), ok(&add_after("greet", "one")), ok(&add_after("one", "two"))]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Accepted);
        assert_eq!(r.content, "pub fn greet() {}\npub fn one() {}\npub fn two() {}\n");
        let statuses: Vec<_> = r.tasks.iter().map(|t| (t.parent, t.status)).collect();
        let parent = r.tasks[0].id;
        assert_eq!(statuses, [
            (None, TaskStatus::Accepted),
            (Some(parent), TaskStatus::Accepted),
            (Some(parent), TaskStatus::Accepted),
        ]);
        assert!(r.prompts[2].starts_with("split-prompt"));
    }

    #[test]
    fn a_stuck_half_stops_the_chain_and_is_never_split_again() {
        // 2 attempts + split + 2 attempts of the first half = 5 calls; the second half is never asked.
        let r = run(vec![ok("Done!"), ok("Nope"), ok(SPLIT), ok("Done!"), ok("Nope")]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Stuck);
        let statuses: Vec<_> = r.tasks.iter().map(|t| t.status).collect();
        assert_eq!(statuses, [TaskStatus::Stuck, TaskStatus::Stuck, TaskStatus::Cancelled]);
        assert_eq!(r.content, START);
    }

    #[test]
    fn the_first_half_stays_saved_when_the_second_gets_stuck() {
        let r = run(vec![ok("Done!"), ok("Nope"), ok(SPLIT), ok(&add_after("greet", "one")), ok("Done!"), ok("Nope")]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Stuck);
        let statuses: Vec<_> = r.tasks.iter().map(|t| t.status).collect();
        assert_eq!(statuses, [TaskStatus::Stuck, TaskStatus::Accepted, TaskStatus::Stuck]);
        assert_eq!(r.content, "pub fn greet() {}\npub fn one() {}\n");
    }

    #[test]
    fn missing_information_goes_straight_to_the_user() {
        let r = run(vec![ok("MISSING: the agreed name")]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Stuck);
        assert_eq!(r.store.attempts(r.tasks[0].id).unwrap().len(), 1);
    }

    #[test]
    fn a_model_that_times_out_is_retried_then_split() {
        let slow = || Err(LlmError::TimedOut { seconds: 1 });
        let r = run(vec![slow(), slow(), slow()]);
        assert_eq!(r.result.unwrap().status, TaskStatus::Stuck);
        let attempts = r.store.attempts(r.tasks[0].id).unwrap();
        assert_eq!(attempts[0].detector, Some(Detector::Timeout));
        assert_eq!(attempts[1].detector, Some(Detector::RepeatError));
    }

    #[test]
    fn an_unreachable_model_stops_at_once_and_the_task_waits() {
        let r = run(vec![Err(LlmError::Unreachable { url: "u".into() })]);
        assert_eq!(r.result, Err(SessionError::ModelDown));
        assert_eq!(r.tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn a_dirty_project_records_nothing() {
        let (dir, _repo) = project(START);
        std::fs::write(dir.path().join("greet.rs"), "unsaved\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        let ask = |_: &str| -> Result<Reply, LlmError> { panic!("the model must not be called") };
        let render = |_: &Message| String::new();
        let events = |_: &Message| {};
        let setup = RunSetup { project: dir.path(), model: "fake", ask: &ask, limits: Limits::default(), check: None, render: &render, events: &events };
        let result = run_ladder(&store, &card(), &setup);
        assert!(matches!(result, Err(SessionError::Run(RunError::Repo(RepoError::Dirty { .. })))));
        assert!(store.tasks(store.project(dir.path()).unwrap()).unwrap().is_empty());
    }
}
