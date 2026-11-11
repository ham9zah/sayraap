//! Turns a core `Message` into text using the Fluent locale files.
//! A key missing from the chosen language falls back to English.

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use sayraap_core::Message;
use unic_langid::LanguageIdentifier;

const EN: &str = include_str!("../../locales/en.ftl");
const AR: &str = include_str!("../../locales/ar.ftl");

pub struct Translator {
    /// Chosen language first, English last.
    bundles: Vec<FluentBundle<FluentResource>>,
}

fn bundle(lang: &str, source: &str) -> FluentBundle<FluentResource> {
    let id: LanguageIdentifier = lang.parse().expect("valid language id");
    let mut bundle = FluentBundle::new(vec![id]);
    // Terminals print the bidi isolation marks as garbage.
    bundle.set_use_isolating(false);
    let resource = match FluentResource::try_new(source.to_string()) {
        Ok(r) => r,
        Err((r, _)) => r, // keep whatever parsed; broken entries fall back
    };
    let _ = bundle.add_resource(resource);
    bundle
}

impl Translator {
    pub fn new(lang: &str) -> Self {
        let mut bundles = Vec::new();
        if lang.starts_with("ar") {
            bundles.push(bundle("ar", AR));
        }
        bundles.push(bundle("en", EN));
        Self { bundles }
    }

    pub fn text(&self, message: &Message) -> String {
        let mut args = FluentArgs::new();
        for (name, value) in &message.args {
            args.set(*name, value.as_str());
        }
        for bundle in &self.bundles {
            if let Some(pattern) = bundle.get_message(message.key).and_then(|m| m.value()) {
                let mut errors = Vec::new();
                return bundle
                    .format_pattern(pattern, Some(&args), &mut errors)
                    .into_owned();
            }
        }
        message.key.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_and_english_differ() {
        let m = sayraap_core::ready();
        let ar = Translator::new("ar").text(&m);
        let en = Translator::new("en").text(&m);
        assert_ne!(ar, en);
        assert!(en.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn missing_key_falls_back_to_english() {
        let m = Message::new("test-english-only");
        assert_eq!(Translator::new("ar").text(&m), "English only");
    }
}
