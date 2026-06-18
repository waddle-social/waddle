use serde::Deserialize;

use crate::bindings::waddle::extension::types;
use crate::ui::display;

const QUOTES_JSON: &str = include_str!("quotes.json");

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct Quote {
    pub(crate) series: String,
    pub(crate) role: String,
    pub(crate) quote: String,
}

pub(crate) fn quote_catalog() -> Result<Vec<Quote>, types::ExtensionError> {
    parse_quotes_json(QUOTES_JSON).map_err(|error| {
        extension_error(
            types::ExtensionErrorCode::TemporaryFailure,
            &format!("stargate quote catalog is invalid: {error}"),
        )
    })
}

pub(crate) fn parse_quotes_json(input: &str) -> Result<Vec<Quote>, serde_json::Error> {
    serde_json::from_str(input)
}

pub(crate) fn select_quote_with_rng(
    quotes: &[Quote],
    mut next_u64: impl FnMut() -> u64,
) -> Result<&Quote, types::ExtensionError> {
    if quotes.is_empty() {
        return Err(extension_error(
            types::ExtensionErrorCode::TemporaryFailure,
            "stargate quote catalog is empty",
        ));
    }
    let len = quotes.len() as u64;
    let threshold = len.wrapping_neg() % len;
    loop {
        let candidate = next_u64();
        if candidate >= threshold {
            return Ok(&quotes[(candidate % len) as usize]);
        }
    }
}

pub(crate) fn quote_body(quote: &Quote) -> String {
    format!("> {}\n\n{}, {}", quote.quote, quote.role, quote.series)
}

pub(crate) fn quote_markup(quote: &Quote) -> Vec<types::MessageMarkupSpan> {
    vec![types::MessageMarkupSpan {
        kind: types::MessageMarkupKind::Blockquote,
        start: 2,
        end: (2 + quote.quote.chars().count()) as u32,
    }]
}

fn extension_error(code: types::ExtensionErrorCode, message: &str) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}
