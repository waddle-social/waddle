//! How a message body reads: Pango markup for the timeline and plain
//! previews for quotes, notifications and the pinned list.
//!
//! XEP-0245 `/me` bodies read as "* Name action", in italics in the
//! timeline. The action keeps its clickable links; the body carries no
//! other markup the core exposes.

use gtk::glib::markup_escape_text;

use crate::composer::me_action;

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

    #[test]
    fn previews_drop_the_command() {
        assert_eq!(preview("Ann", "/me waves"), "* Ann waves");
        assert_eq!(preview("Ann", "hi /me"), "hi /me");
    }
}
