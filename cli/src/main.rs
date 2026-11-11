mod i18n;

use i18n::Translator;
use sayraap_core::check::{Check, DEFAULT_CHECK_TIMEOUT_SECS};
use sayraap_core::limits::Limits;
use sayraap_core::llm::{chat, Endpoint};
use sayraap_core::runner::{read_target, run_task, Verdict};
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

fn path_message(key: &'static str, path: &Path) -> Message {
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

/// `run`: one task card from model to verdict. Accepted means one new commit;
/// rejected means the project is exactly as it was.
fn run(o: &Options, t: &Translator) -> Result<(), Message> {
    let card = load_card(&o.rest[1])?;
    let project = project_translator(o);
    let render = |message: &Message| project.text(message);
    let check = o.check.as_deref().and_then(|line| Check::from_line(line, o.check_timeout));
    let report = run_task(&o.dir, &card, &o.endpoint, o.limits, check.as_ref(), &render)
        .map_err(|e| e.message())?;
    if report.prompt_tokens + report.completion_tokens > 0 {
        usage_line(t, &o.endpoint, report.prompt_tokens, report.completion_tokens);
    }
    match report.verdict {
        Verdict::Accepted(accepted) => {
            let done = Message::new("task-accepted")
                .arg("file", &card.file)
                .num("lines", accepted.changed_lines)
                .arg("commit", &accepted.commit[..7]);
            println!("{}", t.text(&done));
            Ok(())
        }
        Verdict::Rejected(rejection) => {
            eprintln!("{}", t.text(&Message::new("task-rejected")));
            Err(rejection.message())
        }
    }
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
