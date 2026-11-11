//! The core library: no UI and no user-facing text.
//! Everything the user sees leaves this crate as a `Message` (a key plus
//! arguments); the front-ends turn it into text from the locale files.

pub mod llm;

/// A user-facing message, identified by its key in `locales/*.ftl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Message {
    pub fn new(key: &'static str) -> Self {
        Self { key, args: Vec::new() }
    }

    pub fn arg(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.push((name, value.into()));
        self
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
        assert_eq!(m.args[0].0, "version");
    }
}
