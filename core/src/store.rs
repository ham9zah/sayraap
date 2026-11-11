//! The local SQLite database: projects, their tasks, and every attempt.
//! It lives outside the project folder, so recording never dirties the project.

use crate::task::TaskCard;
use crate::{Arg, Message};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = "
CREATE TABLE projects (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
);
CREATE TABLE tasks (
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER NOT NULL REFERENCES projects(id),
    goal        TEXT NOT NULL,
    file        TEXT NOT NULL,
    forbidden   TEXT NOT NULL,
    acceptance  TEXT NOT NULL,
    status      TEXT NOT NULL,
    commit_id   TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE attempts (
    id                 INTEGER PRIMARY KEY,
    task_id            INTEGER NOT NULL REFERENCES tasks(id),
    number             INTEGER NOT NULL,
    model              TEXT NOT NULL,
    outcome            TEXT NOT NULL,
    reason             TEXT,
    reply              TEXT,
    prompt_tokens      INTEGER NOT NULL DEFAULT 0,
    completion_tokens  INTEGER NOT NULL DEFAULT 0,
    started_at         INTEGER NOT NULL,
    finished_at        INTEGER,
    UNIQUE (task_id, number)
);
";

/// The states a task can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Checking,
    Accepted,
    /// Rejected, to be retried.
    Rejected,
    /// Out of options: waits for the user.
    Stuck,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Checking => "checking",
            TaskStatus::Accepted => "accepted",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Stuck => "stuck",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "running" => TaskStatus::Running,
            "checking" => TaskStatus::Checking,
            "accepted" => TaskStatus::Accepted,
            "rejected" => TaskStatus::Rejected,
            "stuck" => TaskStatus::Stuck,
            "cancelled" => TaskStatus::Cancelled,
            _ => TaskStatus::Pending,
        }
    }
}

/// How one attempt ended. `Running` means it has not ended — or the program
/// died in the middle of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Running,
    Accepted,
    Rejected,
    /// The attempt could not be judged (project or tool problem).
    Error,
}

impl AttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptOutcome::Running => "running",
            AttemptOutcome::Accepted => "accepted",
            AttemptOutcome::Rejected => "rejected",
            AttemptOutcome::Error => "error",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "accepted" => AttemptOutcome::Accepted,
            "rejected" => AttemptOutcome::Rejected,
            "error" => AttemptOutcome::Error,
            _ => AttemptOutcome::Running,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: i64,
    pub goal: String,
    pub file: String,
    pub status: TaskStatus,
    pub commit: Option<String>,
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    pub number: i64,
    pub model: String,
    pub outcome: AttemptOutcome,
    /// Why it was rejected, kept as a message key so any language can show it.
    pub reason: Option<Message>,
    /// The model's raw reply, kept to show the user and to detect repetition.
    pub reply: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

/// What is known when an attempt ends.
pub struct AttemptEnd<'a> {
    pub outcome: AttemptOutcome,
    pub reason: Option<&'a Message>,
    pub reply: Option<&'a str>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    pub detail: String,
}

impl StoreError {
    pub fn message(&self) -> Message {
        Message::new("store-failed").arg("detail", &self.detail)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError { detail: e.to_string() }
    }
}

pub struct Store {
    conn: Connection,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn message_to_json(message: &Message) -> String {
    let args: Vec<Value> = message
        .args
        .iter()
        .map(|(name, arg)| match arg {
            Arg::Text(text) => json!([name, text]),
            Arg::Number(number) => json!([name, number]),
        })
        .collect();
    json!({ "key": message.key, "args": args }).to_string()
}

fn message_from_json(text: &str) -> Option<Message> {
    let value: Value = serde_json::from_str(text).ok()?;
    let mut message = Message::new(value["key"].as_str()?);
    for pair in value["args"].as_array()? {
        let name = pair[0].as_str()?;
        message = match &pair[1] {
            Value::Number(n) => message.num(name, n.as_i64()?),
            other => message.arg(name, other.as_str()?),
        };
    }
    Some(message)
}

impl Store {
    /// Opens the database file, creating it and its tables on first use.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::prepare(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            conn.execute_batch(&format!(
                "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
        } else if version != SCHEMA_VERSION {
            return Err(StoreError { detail: format!("unknown database version {version}") });
        }
        Ok(Self { conn })
    }

    /// The id of the project at `path`, registering it on first sight.
    pub fn project(&self, path: &Path) -> Result<i64, StoreError> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path = path.to_string_lossy();
        self.conn.execute(
            "INSERT OR IGNORE INTO projects (path, created_at) VALUES (?1, ?2)",
            params![path, now()],
        )?;
        Ok(self.conn.query_row("SELECT id FROM projects WHERE path = ?1", [path], |r| r.get(0))?)
    }

    /// Records a new task, waiting to run.
    pub fn add_task(&self, project: i64, card: &TaskCard) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO tasks (project_id, goal, file, forbidden, acceptance, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                project,
                card.goal,
                card.file,
                json!(card.forbidden).to_string(),
                json!(card.acceptance).to_string(),
                TaskStatus::Pending.as_str(),
                now()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_status(
        &self,
        task: i64,
        status: TaskStatus,
        commit: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE tasks SET status = ?2, commit_id = COALESCE(?3, commit_id), updated_at = ?4 WHERE id = ?1",
            params![task, status.as_str(), commit, now()],
        )?;
        Ok(())
    }

    /// Opens the next attempt of a task and returns (attempt id, attempt number).
    pub fn start_attempt(&self, task: i64, model: &str) -> Result<(i64, i64), StoreError> {
        let number: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM attempts WHERE task_id = ?1",
            [task],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO attempts (task_id, number, model, outcome, started_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![task, number, model, AttemptOutcome::Running.as_str(), now()],
        )?;
        Ok((self.conn.last_insert_rowid(), number))
    }

    pub fn finish_attempt(&self, attempt: i64, end: &AttemptEnd) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE attempts SET outcome = ?2, reason = ?3, reply = ?4, prompt_tokens = ?5,
                    completion_tokens = ?6, finished_at = ?7 WHERE id = ?1",
            params![
                attempt,
                end.outcome.as_str(),
                end.reason.map(message_to_json),
                end.reply,
                end.prompt_tokens as i64,
                end.completion_tokens as i64,
                now()
            ],
        )?;
        Ok(())
    }

    /// The project's tasks in the order they were added.
    pub fn tasks(&self, project: i64) -> Result<Vec<TaskRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT t.id, t.goal, t.file, t.status, t.commit_id,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id = t.id)
             FROM tasks t WHERE t.project_id = ?1 ORDER BY t.id",
        )?;
        let rows = statement.query_map([project], |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                goal: r.get(1)?,
                file: r.get(2)?,
                status: TaskStatus::parse(&r.get::<_, String>(3)?),
                commit: r.get(4)?,
                attempts: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// True if the task exists and belongs to the project.
    pub fn has_task(&self, project: i64, task: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM tasks WHERE id = ?1 AND project_id = ?2", [task, project], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Every attempt of a task, oldest first.
    pub fn attempts(&self, task: i64) -> Result<Vec<AttemptRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT number, model, outcome, reason, reply, prompt_tokens, completion_tokens,
                    started_at, finished_at
             FROM attempts WHERE task_id = ?1 ORDER BY number",
        )?;
        let rows = statement.query_map([task], |r| {
            Ok(AttemptRow {
                number: r.get(0)?,
                model: r.get(1)?,
                outcome: AttemptOutcome::parse(&r.get::<_, String>(2)?),
                reason: r.get::<_, Option<String>>(3)?.as_deref().and_then(message_from_json),
                reply: r.get(4)?,
                prompt_tokens: r.get(5)?,
                completion_tokens: r.get(6)?,
                started_at: r.get(7)?,
                finished_at: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> TaskCard {
        TaskCard::from_toml("goal = \"g\"\nfile = \"a.rs\"\nforbidden = [\"x\"]\nacceptance = [\"y\"]\n").unwrap()
    }

    #[test]
    fn records_a_task_and_its_attempts() {
        let store = Store::open_in_memory().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let project = store.project(dir.path()).unwrap();
        assert_eq!(store.project(dir.path()).unwrap(), project, "same folder, same project");

        let task = store.add_task(project, &card()).unwrap();
        let (first, n1) = store.start_attempt(task, "local-model").unwrap();
        let reason = Message::new("task-too-large").num("changed", 41).num("limit", 30);
        store
            .finish_attempt(first, &AttemptEnd {
                outcome: AttemptOutcome::Rejected,
                reason: Some(&reason),
                reply: Some("raw reply"),
                prompt_tokens: 400,
                completion_tokens: 60,
            })
            .unwrap();
        let (_second, n2) = store.start_attempt(task, "local-model").unwrap();
        assert_eq!((n1, n2), (1, 2));
        store.set_status(task, TaskStatus::Accepted, Some("abc123")).unwrap();

        let tasks = store.tasks(project).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].status, tasks[0].attempts), (TaskStatus::Accepted, 2));
        assert_eq!(tasks[0].commit.as_deref(), Some("abc123"));

        let attempts = store.attempts(task).unwrap();
        assert_eq!(attempts[0].outcome, AttemptOutcome::Rejected);
        assert_eq!(attempts[0].reason.as_ref(), Some(&reason), "reason survives as key and arguments");
        assert_eq!(attempts[0].reply.as_deref(), Some("raw reply"));
        assert_eq!(attempts[1].outcome, AttemptOutcome::Running, "an unfinished attempt stays visible");
        assert!(store.has_task(project, task).unwrap());
        assert!(!store.has_task(project + 1, task).unwrap());
    }

    #[test]
    fn data_survives_reopening_the_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("nested/sayraap.db");
        let project = {
            let store = Store::open(&file).unwrap();
            let project = store.project(dir.path()).unwrap();
            store.add_task(project, &card()).unwrap();
            project
        };
        let store = Store::open(&file).unwrap();
        assert_eq!(store.tasks(project).unwrap()[0].status, TaskStatus::Pending);
    }
}
