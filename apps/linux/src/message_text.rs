//! How a message body reads: Pango markup for the timeline and plain
//! previews for quotes, notifications and the pinned list.
//!
//! XEP-0245 `/me` bodies read as "* Name action", in italics in the
//! timeline. The action keeps its clickable links; the body carries no
//! other markup the core exposes.

use gtk::glib::markup_escape_text;

use crate::composer::{me_action, ME_PREFIX};
use crate::markup::{self, Span, Style};

/// The body as Pango markup, with its web links clickable.
pub fn body_markup(body: &str) -> String {
    let mut markup = String::with_capacity(body.len());
    let mut rest = body;
    while !rest.is_empty() {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, after) = rest.split_at(word_end);
        let space_end = after
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(after.len());
        let (space, after) = after.split_at(space_end);
        // Trailing punctuation usually ends the sentence, not the link.
        let link_end = word
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'', '"'])
            .len();
        let (link, trailing) = word.split_at(link_end);
        let is_link = (link.starts_with("https://") || link.starts_with("http://"))
            && url::Url::parse(link).is_ok();
        if is_link {
            let escaped = markup_escape_text(link);
            markup.push_str(&format!("<a href=\"{escaped}\">{escaped}</a>"));
            markup.push_str(&markup_escape_text(trailing));
        } else {
            markup.push_str(&markup_escape_text(word));
        }
        markup.push_str(&markup_escape_text(space));
        rest = after;
    }
    markup
}

/// A body as Pango markup: an XEP-0245 action as italic "* sender action",
/// anything else as written.
pub fn message_markup(sender: &str, body: &str) -> String {
    match me_action(body) {
        Some(action) => format!(
            "<i>* {} {}</i>",
            markup_escape_text(sender),
            body_markup(action)
        ),
        None => body_markup(body),
    }
}

/// A body as Pango markup with its XEP-0394 spans drawn: bold, italic,
/// strikethrough, monospace code, code blocks and quotes, and Waddle's
/// link spans. Every piece of text is escaped; only the tags below are
/// ever emitted. Spans are clamped to the body, and without any this is
/// [`message_markup`].
pub fn styled_markup(sender: &str, body: &str, spans: &[Span]) -> String {
    if spans.is_empty() {
        return message_markup(sender, body);
    }
    match me_action(body) {
        Some(action) => {
            let prefix = ME_PREFIX.chars().count();
            let shifted = markup::without_fallback(
                spans.to_vec(),
                body.chars().count(),
                Some((0, u32::try_from(prefix).unwrap_or(0))),
            );
            format!(
                "<i>* {} {}</i>",
                markup_escape_text(sender),
                render(action, &shifted)
            )
        }
        None => render(body, spans),
    }
}

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const STRIKE: u8 = 4;
const CODE: u8 = 8;
const BLOCK: u8 = 16;
const QUOTE: u8 = 32;

/// A span's link target, only for web addresses.
fn web_link(uri: Option<&str>) -> Option<&str> {
    uri.filter(|uri| url::Url::parse(uri).is_ok_and(|url| matches!(url.scheme(), "http" | "https")))
}

fn render(body: &str, spans: &[Span]) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut flags = vec![0u8; chars.len()];
    let mut links: Vec<Option<&str>> = vec![None; chars.len()];
    let mut hidden = vec![false; chars.len()];
    let mut bars = vec![false; chars.len()];
    for span in spans {
        let end = span.end.min(chars.len());
        if span.start >= end {
            continue;
        }
        let range = span.start..end;
        let bit = match span.style {
            Style::Bold => BOLD,
            Style::Italic => ITALIC,
            Style::Strikethrough => STRIKE,
            Style::Code => CODE,
            Style::CodeBlock => CODE | BLOCK,
            Style::Blockquote => QUOTE,
            Style::Link => {
                if let Some(uri) = web_link(span.uri.as_deref()) {
                    for link in &mut links[range.clone()] {
                        link.get_or_insert(uri);
                    }
                }
                0
            }
        };
        for flag in &mut flags[range.clone()] {
            *flag |= bit;
        }
        if span.style == Style::Blockquote {
            // Receivers strip the `>` markers the sender left in the body.
            for at in range.clone() {
                if (at == span.start || chars[at - 1] == '\n') && chars[at] == '>' {
                    hidden[at] = true;
                    bars[at] = true;
                    if at + 1 < end && chars[at + 1] == ' ' {
                        hidden[at + 1] = true;
                    }
                }
            }
        }
    }
    let mut out = String::with_capacity(body.len());
    let mut run = String::new();
    let mut current = (0u8, None);
    for (at, &character) in chars.iter().enumerate() {
        if bars[at] {
            flush(&mut out, &mut run, current);
            out.push_str("<span alpha=\"50%\">\u{258e} </span>");
        }
        if hidden[at] {
            continue;
        }
        let key = (flags[at], links[at]);
        if key != current {
            flush(&mut out, &mut run, current);
            current = key;
        }
        run.push(character);
    }
    flush(&mut out, &mut run, current);
    out
}

/// Close the pending run in the tags its flags name.
fn flush(out: &mut String, run: &mut String, (flags, link): (u8, Option<&str>)) {
    if run.is_empty() {
        return;
    }
    let text = std::mem::take(run);
    let mut open = String::new();
    let mut close = String::new();
    let mut wrap = |start: &str, end: &str| {
        open.push_str(start);
        close.insert_str(0, end);
    };
    if flags & QUOTE != 0 {
        wrap("<span alpha=\"80%\">", "</span>");
    }
    if flags & BLOCK != 0 {
        wrap("<span background=\"#808080\" bgalpha=\"20%\">", "</span>");
    }
    if flags & BOLD != 0 {
        wrap("<b>", "</b>");
    }
    if flags & ITALIC != 0 {
        wrap("<i>", "</i>");
    }
    if flags & STRIKE != 0 {
        wrap("<s>", "</s>");
    }
    if flags & CODE != 0 {
        wrap("<tt>", "</tt>");
    }
    let inner = match link {
        Some(uri) => format!(
            "<a href=\"{}\">{}</a>",
            markup_escape_text(uri),
            markup_escape_text(&text)
        ),
        None if flags & CODE != 0 => markup_escape_text(&text).to_string(),
        None => body_markup(&text),
    };
    out.push_str(&open);
    out.push_str(&inner);
    out.push_str(&close);
}

/// A body as plain text for a quote, notification or list: an action reads
/// "* sender action" rather than showing the `/me` command.
pub fn preview(sender: &str, body: &str) -> String {
    match me_action(body) {
        Some(action) => format!("* {sender} {action}"),
        None => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_read_as_italic_sender_and_action() {
        assert_eq!(
            message_markup("Ann", "/me waves at <everyone>"),
            "<i>* Ann waves at &lt;everyone&gt;</i>"
        );
    }

    #[test]
    fn actions_keep_their_links_clickable() {
        let markup = message_markup("Ann", "/me shares https://waddle.social/x.");
        assert_eq!(
            markup,
            "<i>* Ann shares <a href=\"https://waddle.social/x\">https://waddle.social/x</a>.</i>"
        );
    }

    #[test]
    fn the_sender_cannot_inject_markup() {
        assert_eq!(
            message_markup("<b>x</b>", "/me nods"),
            "<i>* &lt;b&gt;x&lt;/b&gt; nods</i>"
        );
    }

    #[test]
    fn ordinary_bodies_are_not_italic() {
        assert_eq!(message_markup("Ann", "hello & bye"), "hello &amp; bye");
        assert_eq!(message_markup("Ann", "/meshrugs"), "/meshrugs");
        assert_eq!(message_markup("Ann", "/ME waves"), "/ME waves");
    }

    fn span(style: Style, start: usize, end: usize) -> Span {
        Span {
            style,
            start,
            end,
            uri: None,
        }
    }

    #[test]
    fn spans_draw_as_tags_and_escape_every_character() {
        let spans = [span(Style::Bold, 0, 4), span(Style::Code, 8, 12)];
        assert_eq!(
            styled_markup("Ann", "<b>x & y <i>z", &spans),
            "<b>&lt;b&gt;x</b> &amp; y<tt> &lt;i&gt;</tt>z"
        );
    }

    #[test]
    fn spans_past_the_body_are_clamped_and_empty_ones_ignored() {
        let spans = [span(Style::Italic, 1, 99), span(Style::Bold, 5, 5)];
        assert_eq!(styled_markup("Ann", "abc", &spans), "a<i>bc</i>");
    }

    #[test]
    fn quotes_lose_their_markers_and_code_keeps_urls_inert() {
        let spans = [span(Style::Blockquote, 0, 11), span(Style::Code, 12, 32)];
        let markup = styled_markup("Ann", "> one\n> two\nhttps://waddle.social", &spans);
        assert!(markup.starts_with("<span alpha=\"50%\">\u{258e} </span>"));
        assert!(!markup.contains("&gt;"), "{markup}");
        assert!(!markup.contains("<a "), "{markup}");
    }

    #[test]
    fn link_spans_link_only_web_addresses() {
        let link = |uri: &str| {
            [Span {
                uri: Some(uri.into()),
                ..span(Style::Link, 0, 2)
            }]
        };
        assert_eq!(
            styled_markup("Ann", "hi", &link("https://a.example/?q=1&r=2")),
            "<a href=\"https://a.example/?q=1&amp;r=2\">hi</a>"
        );
        assert_eq!(
            styled_markup("Ann", "hi", &link("javascript:alert(1)")),
            "hi"
        );
    }

    #[test]
    fn actions_keep_their_spans() {
        let spans = [span(Style::Bold, 4, 9)];
        assert_eq!(
            styled_markup("Ann", "/me waves hi", &spans),
            "<i>* Ann <b>waves</b> hi</i>"
        );
    }

    #[test]
    fn previews_drop_the_command() {
        assert_eq!(preview("Ann", "/me waves"), "* Ann waves");
        assert_eq!(preview("Ann", "hi /me"), "hi /me");
    }
}
