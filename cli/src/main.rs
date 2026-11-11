mod i18n;

use sayraap_core::llm::{chat, Endpoint};
use sayraap_core::Message;
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_URL: &str = "http://localhost:11434/v1";
const DEFAULT_MODEL: &str = "qwen2.5-coder:7b";
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Command-line options. Each falls back to an environment variable, then a default.
struct Options {
    lang: String,
    url: String,
    model: String,
    timeout: Duration,
    /// Everything that is not an option: the command and its arguments.
    rest: Vec<String>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn options() -> Options {
    let mut o = Options {
        lang: env_or("LANG", "en"),
        url: env_or("SAYRAAP_URL", DEFAULT_URL),
        model: env_or("SAYRAAP_MODEL", DEFAULT_MODEL),
        timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        rest: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" => o.lang = args.next().unwrap_or(o.lang),
            "--url" => o.url = args.next().unwrap_or(o.url),
            "--model" => o.model = args.next().unwrap_or(o.model),
            "--timeout" => {
                if let Some(secs) = args.next().and_then(|v| v.parse().ok()) {
                    o.timeout = Duration::from_secs(secs);
                }
            }
            _ => o.rest.push(arg),
        }
    }
    o
}

fn main() -> ExitCode {
    let o = options();
    let t = i18n::Translator::new(&o.lang);

    match o.rest.first().map(String::as_str) {
        None => {
            println!("{}", t.text(&sayraap_core::ready()));
            ExitCode::SUCCESS
        }
        Some("ask") if o.rest.len() > 1 => {
            let endpoint = Endpoint { base_url: o.url, model: o.model, timeout: o.timeout };
            match chat(&endpoint, &o.rest[1..].join(" ")) {
                Ok(reply) => {
                    println!("{}", reply.text);
                    let usage = Message::new("llm-tokens")
                        .arg("model", &endpoint.model)
                        .arg("prompt", reply.prompt_tokens.to_string())
                        .arg("completion", reply.completion_tokens.to_string());
                    eprintln!("{}", t.text(&usage));
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("{}", t.text(&error.message()));
                    ExitCode::FAILURE
                }
            }
        }
        Some(_) => {
            eprintln!("{}", t.text(&Message::new("cli-usage")));
            ExitCode::FAILURE
        }
    }
}
