//! Talks to any OpenAI-compatible chat endpoint (Ollama, LM Studio, ...).
//! One request, one reply, no conversation history.

use crate::Message;
use serde_json::{json, Value};
use std::time::Duration;

/// Where a model lives and how long we are willing to wait for it.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Base URL ending in `/v1`, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    /// No connection: server down or wrong address.
    Unreachable { url: String },
    /// The model did not finish answering within the task's time limit.
    TimedOut { seconds: u64 },
    /// The server answered with an HTTP error status.
    Http { status: u16 },
    /// The answer was not the JSON shape we expect.
    BadResponse,
}

impl LlmError {
    pub fn message(&self) -> Message {
        match self {
            LlmError::Unreachable { url } => Message::new("llm-unreachable").arg("url", url),
            LlmError::TimedOut { seconds } => Message::new("llm-timeout").num("seconds", *seconds),
            LlmError::Http { status } => {
                Message::new("llm-http-error").num("status", *status)
            }
            LlmError::BadResponse => Message::new("llm-bad-response"),
        }
    }
}

/// Sends one prompt in a fresh context and returns the model's reply.
pub fn chat(endpoint: &Endpoint, prompt: &str) -> Result<Reply, LlmError> {
    let url = format!("{}/chat/completions", endpoint.base_url.trim_end_matches('/'));
    let body = json!({
        "model": endpoint.model,
        "messages": [{ "role": "user", "content": prompt }],
        "temperature": 0,
        "stream": false,
    });
    let agent = ureq::AgentBuilder::new().timeout(endpoint.timeout).build();
    let response = match agent.post(&url).send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(status, _)) => return Err(LlmError::Http { status }),
        Err(ureq::Error::Transport(transport)) => {
            return Err(if is_timeout(&transport) {
                LlmError::TimedOut { seconds: endpoint.timeout.as_secs() }
            } else {
                LlmError::Unreachable { url }
            })
        }
    };
    // The time limit can also pass while the body is still arriving.
    let value: Value = response.into_json().map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            LlmError::TimedOut { seconds: endpoint.timeout.as_secs() }
        }
        _ => LlmError::BadResponse,
    })?;
    parse_reply(&value)
}

/// True if an I/O timeout is anywhere in the error's chain of causes.
fn is_timeout(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(e) = cause {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
                return true;
            }
        }
        cause = e.source();
    }
    false
}

fn parse_reply(value: &Value) -> Result<Reply, LlmError> {
    let text = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or(LlmError::BadResponse)?
        .to_string();
    Ok(Reply {
        text,
        prompt_tokens: value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        completion_tokens: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_normal_reply() {
        let value = json!({
            "choices": [{ "message": { "role": "assistant", "content": "hi" } }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 1 }
        });
        let reply = parse_reply(&value).unwrap();
        assert_eq!(reply.text, "hi");
        assert_eq!((reply.prompt_tokens, reply.completion_tokens), (3, 1));
    }

    #[test]
    fn rejects_a_reply_without_content() {
        assert_eq!(parse_reply(&json!({ "choices": [] })), Err(LlmError::BadResponse));
    }

    #[test]
    fn a_silent_server_is_a_timeout() {
        // Accepts the connection and never answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = Endpoint {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            model: "none".into(),
            timeout: Duration::from_secs(1),
        };
        assert_eq!(chat(&endpoint, "x"), Err(LlmError::TimedOut { seconds: 1 }));
    }

    #[test]
    fn closed_port_is_unreachable() {
        let endpoint = Endpoint {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "none".into(),
            timeout: Duration::from_secs(2),
        };
        assert!(matches!(chat(&endpoint, "x"), Err(LlmError::Unreachable { .. })));
    }
}
