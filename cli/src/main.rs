mod i18n;

use i18n::Translator;
use sayraap_core::check::{Check, DEFAULT_CHECK_TIMEOUT_SECS};
use sayraap_core::limits::Limits;
use sayraap_core::llm::{chat, Endpoint};
use sayraap_core::runner::read_target;
use sayraap_core::session::{run_ladder, RunSetup};
use sayraap_core::store::{AttemptKind, Store, TaskStatus};
use sayraap_core::task::TaskCard;
use sayraap_core::Message;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_URL: &str = "http://localhost:11434/v1";
const DEFAULT_MODEL: &str = "qwen2.5-coder:7b";
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Command-line options. Each falls back to an environment variable, then a default.
struct Options {
    /// Language of what the user reads.
    lang: String,
    /// Language of the task card sent to the model. Defaults to `lang`.
    project_lang: Option<String>,
    /// Project root that the card's file path is relative to.
    dir: PathBuf,
    endpoint: Endpoint,
    limits: Limits,
    /// Command that must succeed before a change is accepted, e.g. "cargo check".
    check: Option<String>,
    check_timeout: Duration,
    /// Database file. Defaults to the user's application-data folder.
    db: Option<PathBuf>,
    /// With `attempts`: also print each raw model reply.
    show_reply: bool,
    /// Everything that is not an option: the command and its arguments.
    rest: Vec<String>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn options() -> Options {
    let mut o = Options {
        lang: env_or("LANG", "en"),
        project_lang: None,
        dir: PathBuf::from("."),
        endpoint: Endpoint {
            base_url: env_or("SAYRAAP_URL", DEFAULT_URL),
            model: env_or("SAYRAAP_MODEL", DEFAULT_MODEL),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        },
        limits: Limits::default(),
        check: None,
        check_timeout: Duration::from_secs(DEFAULT_CHECK_TIMEOUT_SECS),
        db: std::env::var("SAYRAAP_DB").ok().map(PathBuf::from),
        show_reply: false,
        rest: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" => o.lang = args.next().unwrap_or(o.lang),
            "--project-lang" => o.project_lang = args.next(),
            "--dir" => o.dir = args.next().map(PathBuf::from).unwrap_or(o.dir),
            "--url" => o.endpoint.base_url = args.next().unwrap_or(o.endpoint.base_url),
            "--model" => o.endpoint.model = args.next().unwrap_or(o.endpoint.model),
            "--timeout" => {
                if let Some(secs) = args.next().and_then(|v| v.parse().ok()) {
                    o.endpoint.timeout = Duration::from_secs(secs);
                }
            }
            "--show-reply" => o.show_reply = true,
            "--db" => o.db = args.next().map(PathBuf::from),
            "--check" => o.check = args.next(),
            "--check-timeout" => {
                if let Some(secs) = args.next().and_then(|v| v.parse().ok()) {
                    o.check_timeout = Duration::from_secs(secs);
                }
            }
            "--max-lines" => {
                if let Some(lines) = args.next().and_then(|v| v.parse().ok()) {
                    o.limits.max_changed_lines = lines;
                }
            }
            _ => o.rest.push(arg),
        }
    }
    o
}

fn usage_line(t: &Translator, endpoint: &Endpoint, prompt: u64, completion: u64) {
    let usage = Message::new("llm-tokens")
        .arg("model", &endpoint.model)
        .num("prompt", prompt)
        .num("completion", completion);
    eprintln!("{}", t.text(&usage));
}

fn path_message(key: &str, path: &Path) -> Message {
    Message::new(key).arg("path", path.display().to_string())
}

fn load_card(card_path: &str) -> Result<TaskCard, Message> {
    let card_path = Path::new(card_path);
    let source = std::fs::read_to_string(card_path)
        .map_err(|_| path_message("file-unreadable", card_path))?;
    TaskCard::from_toml(&source).map_err(|e| e.message())
}

/// The translator for text sent to the model, which may differ from the UI language.
fn project_translator(o: &Options) -> Translator {
    Translator::new(o.project_lang.as_deref().unwrap_or(&o.lang))
}

/// `ask`: send a free-form question and print the answer.
fn ask(o: &Options, t: &Translator) -> Result<(), Message> {
    let reply = chat(&o.endpoint, &o.rest[1..].join(" ")).map_err(|e| e.message())?;
    println!("{}", reply.text);
    usage_line(t, &o.endpoint, reply.prompt_tokens, reply.completion_tokens);
    Ok(())
}

/// `prompt`: print the card exactly as the model would receive it. Calls no model.
fn show_prompt(o: &Options) -> Result<(), Message> {
    let card = load_card(&o.rest[1])?;
    let current = read_target(&o.dir, &card).map_err(|e| e.message())?;
    println!("{}", project_translator(o).text(&card.prompt(&current)));
    Ok(())
}

/// Opens the database, outside the project so recording never dirties it.
fn open_store(o: &Options) -> Result<Store, Message> {
    let path = match &o.db {
        Some(path) => path.clone(),
        None => dirs::data_dir()
            .ok_or_else(|| Message::new("store-no-folder"))?
            .join("sayraap")
            .join("sayraap.db"),
    };
    Store::open(&path).map_err(|e| e.message())
}

/// `run`: one task card taken up the escalation ladder until it is accepted
/// or stuck. Progress is printed as it happens and everything is recorded.
fn run(o: &Options, t: &Translator) -> Result<(), Message> {
    let card = load_card(&o.rest[1])?;
    let store = open_store(o)?;
    let project = project_translator(o);
    let render = |message: &Message| project.text(message);
    let events = |message: &Message| eprintln!("{}", t.text(message));
    let ask = |prompt: &str| chat(&o.endpoint, prompt);
    let check = o.check.as_deref().and_then(|line| Check::from_line(line, o.check_timeout));
    let setup = RunSetup {
        project: &o.dir,
        model: &o.endpoint.model,
        ask: &ask,
        limits: o.limits,
        check: check.as_ref(),
        render: &render,
        events: &events,
    };
    let result = run_ladder(&store, &card, &setup).map_err(|e| e.message())?;
    let summary = Message::new(match result.status {
        TaskStatus::Accepted => "run-accepted",
        _ => "run-stuck",
    })
    .num("task", result.task);
    if result.status == TaskStatus::Accepted {
        println!("{}", t.text(&summary));
        Ok(())
    } else {
        Err(summary)
    }
}

/// `tasks`: the project's recorded tasks, oldest first.
fn tasks(o: &Options, t: &Translator) -> Result<(), Message> {
    let store = open_store(o)?;
    let project = store.project(&o.dir).map_err(|e| e.message())?;
    let rows = store.tasks(project).map_err(|e| e.message())?;
    if rows.is_empty() {
        println!("{}", t.text(&Message::new("tasks-empty")));
    }
    for row in rows {
        let status = t.text(&Message::new(format!("status-{}", row.status.as_str())));
        let line = Message::new("task-line")
            .num("id", row.id)
            .arg("status", status)
            .arg("file", row.file)
            .num("attempts", row.attempts)
            .arg("commit", row.commit.as_deref().map(|c| &c[..c.len().min(7)]).unwrap_or("-"))
            .arg("parent", row.parent.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()))
            .arg("goal", row.goal);
        println!("{}", t.text(&line));
    }
    Ok(())
}

/// `attempts <task>`: every attempt of one task, with the reason for each rejection.
fn attempts(o: &Options, t: &Translator) -> Result<(), Message> {
    let unknown = || Message::new("task-not-found").arg("id", &o.rest[1]);
    let task: i64 = o.rest[1].parse().map_err(|_| unknown())?;
    let store = open_store(o)?;
    let project = store.project(&o.dir).map_err(|e| e.message())?;
    if !store.has_task(project, task).map_err(|e| e.message())? {
        return Err(unknown());
    }
    for row in store.attempts(task).map_err(|e| e.message())? {
        let outcome = t.text(&Message::new(format!("outcome-{}", row.outcome.as_str())));
        let line = Message::new(match row.kind {
            AttemptKind::Execute => "attempt-line",
            AttemptKind::Split => "attempt-line-split",
        })
            .num("number", row.number)
            .arg("model", row.model)
            .arg("outcome", outcome)
            .num("prompt", row.prompt_tokens)
            .num("completion", row.completion_tokens)
            .num("seconds", row.finished_at.map(|end| end - row.started_at).unwrap_or(0));
        println!("{}", t.text(&line));
        if let Some(reason) = &row.reason {
            for text in t.text(reason).lines() {
                println!("    {text}");
            }
        }
        if let Some(detector) = row.detector {
            println!("    {}", t.text(&Message::new(format!("detector-{}", detector.as_str()))));
        }
        if o.show_reply {
            for text in row.reply.as_deref().unwrap_or("").lines() {
                println!("    | {text}");
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let o = options();
    let t = Translator::new(&o.lang);

    let result = match (o.rest.first().map(String::as_str), o.rest.len()) {
        (None, _) => {
            println!("{}", t.text(&sayraap_core::ready()));
            Ok(())
        }
        (Some("ask"), n) if n > 1 => ask(&o, &t),
        (Some("prompt"), 2) => show_prompt(&o),
        (Some("run"), 2) => run(&o, &t),
        (Some("tasks"), 1) => tasks(&o, &t),
        (Some("attempts"), 2) => attempts(&o, &t),
        _ => Err(Message::new("cli-usage")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{}", t.text(&message));
            ExitCode::FAILURE
        }
    }
}
