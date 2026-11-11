//! The core library: no UI and no user-facing text.
//! Everything the user sees leaves this crate as a `Message` (a key plus
//! arguments); the front-ends turn it into text from the locale files.

pub mod check;
pub mod edit;
pub mod limits;
pub mod llm;
pub mod repo;
pub mod runner;
pub mod session;
pub mod store;
pub mod task;

/// One argument of a message. Numbers are kept as numbers so a language
/// can pick the right plural form for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Text(String),
    Number(i64),
}

/// A user-facing message, identified by its key in `locales/*.ftl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub key: String,
    pub args: Vec<(String, Arg)>,
}

impl Message {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), args: Vec::new() }
    }

    /// Adds a text argument.
    pub fn arg(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.push((name.into(), Arg::Text(value.into())));
        self
    }

    /// Adds a numeric argument.
    pub fn num(mut self, name: impl Into<String>, value: impl TryInto<i64>) -> Self {
        self.args.push((name.into(), Arg::Number(value.try_into().unwrap_or(i64::MAX))));
        self
    }

    /// The text argument called `name`, if there is one.
    pub fn text_arg(&self, name: &str) -> Option<&str> {
        self.args.iter().find_map(|(n, a)| match a {
            Arg::Text(t) if n == name => Some(t.as_str()),
            _ => None,
        })
    }
}

/// The greeting shown when the program starts.
pub fn ready() -> Message {
    Message::new("core-ready").arg("version", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_carries_version() {
        let m = ready();
        assert_eq!(m.key, "core-ready");
        assert!(m.text_arg("version").is_some());
    }
}
