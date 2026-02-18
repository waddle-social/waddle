use std::collections::HashMap;

use fluent::{FluentArgs, FluentBundle, FluentResource};
use fluent_langneg::{NegotiationStrategy, negotiate_languages};
use unic_langid::LanguageIdentifier;

#[derive(Debug, thiserror::Error)]
pub enum I18nError {
    #[error("failed to parse FTL for locale {locale}: {reason}")]
    ParseFailed { locale: String, reason: String },

    #[error("locale not available: {0}")]
    LocaleNotAvailable(String),
}

struct LocaleData {
    main: &'static str,
    errors: &'static str,
    commands: &'static str,
}

const AVAILABLE_LOCALES: &[&str] = &["en-US"];

fn builtin_ftl(locale: &str) -> Option<LocaleData> {
    match locale {
        "en-US" => Some(LocaleData {
            main: include_str!("../locales/en-US/main.ftl"),
            errors: include_str!("../locales/en-US/errors.ftl"),
            commands: include_str!("../locales/en-US/commands.ftl"),
        }),
        _ => None,
    }
}

pub struct I18n {
    current_locale: String,
    bundles: HashMap<String, FluentBundle<FluentResource>>,
}

impl I18n {
    pub fn new(preferred: Option<&str>, available: &[&str]) -> Self {
        let sys_locale = sys_locale::get_locale();
        let requested_strs: Vec<&str> = [preferred, sys_locale.as_deref()]
            .into_iter()
            .flatten()
            .collect();

        let effective_available = if available.is_empty() {
            AVAILABLE_LOCALES
        } else {
            available
        };

        let mut bundles = HashMap::new();
        for &locale_str in effective_available {
            if let Some(bundle) = Self::build_bundle(locale_str) {
                bundles.insert(locale_str.to_string(), bundle);
            }
        }

        if !bundles.contains_key("en-US") {
            if let Some(bundle) = Self::build_bundle("en-US") {
                bundles.insert("en-US".to_string(), bundle);
            }
        }

        let mut negotiated_available: Vec<&str> = effective_available
            .iter()
            .copied()
            .filter(|locale| bundles.contains_key(*locale))
            .collect();

        if bundles.contains_key("en-US")
            && !negotiated_available
                .iter()
                .any(|locale| locale.eq_ignore_ascii_case("en-US"))
        {
            negotiated_available.push("en-US");
        }

        let negotiated = Self::negotiate(&requested_strs, &negotiated_available);
        let current_locale = negotiated
            .first()
            .copied()
            .or_else(|| negotiated_available.first().copied())
            .unwrap_or("en-US")
            .to_string();

        Self {
            current_locale,
            bundles,
        }
    }

    pub fn t(&self, message_id: &str, args: Option<&FluentArgs>) -> String {
        if let Some(bundle) = self.bundles.get(&self.current_locale) {
            if let Some(msg) = bundle.get_message(message_id) {
                if let Some(pattern) = msg.value() {
                    let mut errors = vec![];
                    let value = bundle.format_pattern(pattern, args, &mut errors);
                    return value.into_owned();
                }
            }
        }
        message_id.to_string()
    }

    pub fn current_locale(&self) -> &str {
        &self.current_locale
    }

    pub fn available_locales(&self) -> Vec<String> {
        let mut locales: Vec<String> = self.bundles.keys().cloned().collect();
        locales.sort();
        locales
    }

    pub fn add_messages(&mut self, locale: &str, ftl_content: &str) -> Result<(), I18nError> {
        let bundle = self
            .bundles
            .get_mut(locale)
            .ok_or_else(|| I18nError::LocaleNotAvailable(locale.to_string()))?;

        let resource = FluentResource::try_new(ftl_content.to_string()).map_err(|(_, errs)| {
            I18nError::ParseFailed {
                locale: locale.to_string(),
                reason: errs
                    .iter()
                    .map(|e| format!("{e:?}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        })?;

        bundle.add_resource_overriding(resource);
        Ok(())
    }

    fn negotiate<'a>(requested: &[&str], available: &'a [&str]) -> Vec<&'a str> {
        let requested_langids: Vec<fluent_langneg::LanguageIdentifier> =
            requested.iter().filter_map(|s| s.parse().ok()).collect();
        let available_langids: Vec<fluent_langneg::LanguageIdentifier> =
            available.iter().filter_map(|s| s.parse().ok()).collect();
        let default: Option<fluent_langneg::LanguageIdentifier> = "en-US".parse().ok();

        let negotiated = negotiate_languages(
            &requested_langids,
            &available_langids,
            default.as_ref(),
            NegotiationStrategy::Filtering,
        );

        negotiated
            .into_iter()
            .filter_map(|langid| {
                let s = langid.to_string();
                available.iter().find(|&&a| a.eq_ignore_ascii_case(&s))
            })
            .copied()
            .collect()
    }

    fn build_bundle(locale_str: &str) -> Option<FluentBundle<FluentResource>> {
        let data = builtin_ftl(locale_str)?;
        let langid: LanguageIdentifier = locale_str.parse().ok()?;
        let mut bundle = FluentBundle::new(vec![langid]);
        bundle.set_use_isolating(false);

        for ftl_src in [data.main, data.errors, data.commands] {
            let resource = FluentResource::try_new(ftl_src.to_string()).ok()?;
            let _ = bundle.add_resource(resource);
        }

        Some(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_locale_is_en_us() {
        let i18n = I18n::new(None, &[]);
        assert_eq!(i18n.current_locale(), "en-US");
    }

    #[test]
    fn explicit_locale_preference() {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        assert_eq!(i18n.current_locale(), "en-US");
    }

    #[test]
    fn resolves_known_message() {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        assert_eq!(i18n.t("app-name", None), "Waddle");
    }

    #[test]
    fn resolves_error_message() {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        assert_eq!(i18n.t("error-auth-failed", None), "Authentication failed");
    }

    #[test]
    fn resolves_command_message() {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        assert_eq!(i18n.t("cmd-quit", None), "Quit Waddle");
    }

    #[test]
    fn returns_message_id_for_missing_key() {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        assert_eq!(i18n.t("nonexistent-key", None), "nonexistent-key");
    }

    #[test]
    fn resolves_message_with_args() {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        let mut args = FluentArgs::new();
        args.set("reason", "server unreachable");
        let result = i18n.t("error-connection-failed", Some(&args));
        assert_eq!(result, "Connection failed: server unreachable");
    }

    #[test]
    fn available_locales_lists_all() {
        let i18n = I18n::new(None, &["en-US"]);
        let locales = i18n.available_locales();
        assert!(locales.contains(&"en-US".to_string()));
    }

    #[test]
    fn add_messages_extends_bundle() {
        let mut i18n = I18n::new(Some("en-US"), &["en-US"]);
        let ftl = "plugin-greeting = Hello from plugin!";
        i18n.add_messages("en-US", ftl).unwrap();
        assert_eq!(i18n.t("plugin-greeting", None), "Hello from plugin!");
    }

    #[test]
    fn add_messages_overrides_existing() {
        let mut i18n = I18n::new(Some("en-US"), &["en-US"]);
        let ftl = "app-name = Waddle Override";
        i18n.add_messages("en-US", ftl).unwrap();
        assert_eq!(i18n.t("app-name", None), "Waddle Override");
    }

    #[test]
    fn add_messages_to_unavailable_locale_fails() {
        let mut i18n = I18n::new(Some("en-US"), &["en-US"]);
        let err = i18n.add_messages("fr", "key = value").unwrap_err();
        assert!(matches!(err, I18nError::LocaleNotAvailable(_)));
    }

    #[test]
    fn add_messages_with_invalid_ftl_fails() {
        let mut i18n = I18n::new(Some("en-US"), &["en-US"]);
        let err = i18n.add_messages("en-US", "= invalid ftl").unwrap_err();
        assert!(matches!(err, I18nError::ParseFailed { .. }));
    }

    #[test]
    fn falls_back_to_en_us_for_unknown_locale() {
        let i18n = I18n::new(Some("xx-YY"), &["en-US"]);
        assert_eq!(i18n.current_locale(), "en-US");
    }

    #[test]
    fn falls_back_to_en_us_when_preferred_locale_has_no_bundle() {
        let i18n = I18n::new(Some("de"), &["de", "en-US"]);
        assert_eq!(i18n.current_locale(), "en-US");
        assert_eq!(i18n.t("app-name", None), "Waddle");
    }

    #[test]
    fn falls_back_to_en_us_when_available_has_no_loadable_locales() {
        let i18n = I18n::new(Some("de"), &["de"]);
        assert_eq!(i18n.current_locale(), "en-US");
        assert_eq!(i18n.t("app-name", None), "Waddle");
    }
}
