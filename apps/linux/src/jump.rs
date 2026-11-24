//! Jumping to a message that may not be loaded yet: a pinned message or a
//! search result. The conversation pages older history in until the
//! message appears, the archive runs out, or the page budget does.

use waddle_client_session::TimelineItem;

/// Older pages one jump may load before it gives up and says so.
pub const JUMP_PAGES: u8 = 20;

/// The row for an archive stanza id.
pub fn position(items: &[TimelineItem], target: &str) -> Option<usize> {
    items.iter().position(|item| {
        item.key.as_str() == target || item.reference_id.as_deref() == Some(target)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// It is in the timeline at this row.
    Found(usize),
    /// Page in older history, then ask again.
    LoadOlder,
    /// It is not in what the archive returns.
    Missing,
}

/// What a jump to `target` does next.
pub fn next_step(
    items: &[TimelineItem],
    target: &str,
    history_complete: bool,
    pages_left: u8,
) -> Step {
    if let Some(at) = position(items, target) {
        return Step::Found(at);
    }
    if history_complete || pages_left == 0 {
        Step::Missing
    } else {
        Step::LoadOlder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_client_session::{HistoryPage, IncomingMessage, Timeline};

    fn items(ids: &[&str]) -> Vec<TimelineItem> {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            ids.iter()
                .enumerate()
                .map(|(at, id)| IncomingMessage {
                    stanza_id: Some((*id).into()),
                    reference_id: Some((*id).into()),
                    sender: "ann".into(),
                    body: Some("hi".into()),
                    timestamp_ms: i64::try_from(at).unwrap_or(0) + 1,
                    ..IncomingMessage::default()
                })
                .collect(),
            ids.first().map(|id| (*id).to_string()),
            HistoryPage::Partial,
        );
        timeline.items().to_vec()
    }

    #[test]
    fn a_loaded_message_is_found_by_its_stanza_id() {
        let items = items(&["a", "b", "c"]);
        assert_eq!(position(&items, "b"), Some(1));
        assert_eq!(next_step(&items, "c", false, 3), Step::Found(2));
    }

    #[test]
    fn an_unloaded_message_pages_older_history_in() {
        let items = items(&["b", "c"]);
        assert_eq!(next_step(&items, "a", false, 5), Step::LoadOlder);
    }

    #[test]
    fn it_gives_up_when_the_archive_or_the_budget_runs_out() {
        let items = items(&["b", "c"]);
        assert_eq!(next_step(&items, "a", true, 5), Step::Missing);
        assert_eq!(next_step(&items, "a", false, 0), Step::Missing);
    }
}
