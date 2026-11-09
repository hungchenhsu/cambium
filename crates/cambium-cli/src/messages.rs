//! Human-facing messages from a Fluent catalog compiled into the binary (ADR-0005).

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use unic_langid::LanguageIdentifier;

const ENGLISH: &str = include_str!("../locales/en/cli.ftl");

pub struct Catalog {
    bundle: FluentBundle<FluentResource>,
}

impl Catalog {
    pub fn english() -> Self {
        let resource =
            FluentResource::try_new(ENGLISH.to_owned()).expect("the English catalog parses");
        let locale: LanguageIdentifier = "en".parse().expect("a valid locale");
        let mut bundle = FluentBundle::new(vec![locale]);
        // Isolation marks would reach terminals and logs as invisible characters.
        bundle.set_use_isolating(false);
        bundle
            .add_resource(resource)
            .expect("the English catalog has no duplicate messages");
        Self { bundle }
    }

    /// Formats a message. A missing message is a bug, so it is shown by its ID.
    pub fn format(&self, id: &str, args: &[(&str, &str)]) -> String {
        let Some(pattern) = self.bundle.get_message(id).and_then(|m| m.value()) else {
            return id.to_owned();
        };
        let mut values = FluentArgs::new();
        for (name, value) in args {
            values.set(*name, value.to_string());
        }
        let mut errors = Vec::new();
        self.bundle
            .format_pattern(pattern, Some(&values), &mut errors)
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_used_by_the_code_exists_and_formats_without_errors() {
        let catalog = Catalog::english();
        let source = include_str!("lib.rs");
        let mut used: Vec<_> = source
            .match_indices(".format(")
            .filter_map(|(index, _)| {
                let rest = &source[index..];
                let start = rest.find('"')? + 1;
                let end = start + rest[start..].find('"')?;
                Some(rest[start..end].to_owned())
            })
            .collect();
        used.sort();
        used.dedup();
        assert!(used.len() >= 10, "{used:?}");
        for id in used {
            let message = catalog.bundle.get_message(&id);
            assert!(message.is_some(), "missing message {id}");
            let mut errors = Vec::new();
            let mut args = FluentArgs::new();
            for name in [
                "code",
                "status",
                "message",
                "request_id",
                "path",
                "reason",
                "source",
                "url",
            ] {
                args.set(name, "x");
            }
            catalog.bundle.format_pattern(
                message.unwrap().value().unwrap(),
                Some(&args),
                &mut errors,
            );
            assert!(errors.is_empty(), "{id}: {errors:?}");
        }
    }

    #[test]
    fn output_has_no_unicode_isolation_marks() {
        let text = Catalog::english().format(
            "api-error",
            &[
                ("code", "not_found"),
                ("status", "404"),
                ("message", "Resource not found"),
            ],
        );
        assert_eq!(text, "Error not_found (HTTP 404): Resource not found");
    }
}
