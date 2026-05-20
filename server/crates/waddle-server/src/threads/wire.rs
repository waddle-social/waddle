//! XML wire shape for `urn:waddle:threads:0`. Built via `minidom::Element`
//! so no XML is ever concatenated as strings (CLAUDE.md hard rule).

use minidom::Element;
use xmpp_parsers::iq::Iq;

use super::query::{ThreadEntry, ThreadsError, ThreadsPage, ThreadsQuery, NS_THREADS};

/// XEP-0059 Result Set Management namespace.
const NS_RSM: &str = "http://jabber.org/protocol/rsm";

/// Parse a `<query xmlns='urn:waddle:threads:0'/>` IQ payload into a
/// `ThreadsQuery`. The IQ MUST be a `get` for this to succeed.
pub fn parse_threads_query(iq: &Iq) -> Result<ThreadsQuery, ThreadsError> {
    let payload = match iq {
        Iq::Get { payload: el, .. } => el,
        _ => return Err(ThreadsError::WrongIqType),
    };
    if !payload.is("query", NS_THREADS) {
        return Err(ThreadsError::ExpectedElement("query"));
    }

    let mut q = ThreadsQuery::default();
    if let Some(rsm) = payload.get_child("set", NS_RSM) {
        if let Some(max_el) = rsm.get_child("max", NS_RSM) {
            let text = max_el.text();
            let parsed: u32 = text
                .trim()
                .parse()
                .map_err(|_| ThreadsError::InvalidInteger(text.clone()))?;
            q.page_size = Some(parsed);
        }
        if let Some(after_el) = rsm.get_child("after", NS_RSM) {
            let text = after_el.text();
            if !text.is_empty() {
                q.after_cursor = Some(text);
            }
        }
    }
    Ok(q)
}

/// Build the `<threads>` response element for `page`.
pub fn build_threads_response(page: &ThreadsPage) -> Element {
    let mut threads = Element::builder("threads", NS_THREADS)
        .attr(
            minidom::rxml::xml_ncname!("total").to_owned(),
            page.total.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("unread-threads").to_owned(),
            page.unread_threads.to_string(),
        )
        .build();

    for entry in &page.entries {
        threads.append_child(build_thread_entry(entry));
    }

    let mut set = Element::builder("set", NS_RSM).build();
    if let Some(ref first) = page.first_cursor {
        let mut first_el = Element::builder("first", NS_RSM).build();
        first_el.append_text_node(first);
        set.append_child(first_el);
    }
    if let Some(ref last) = page.last_cursor {
        let mut last_el = Element::builder("last", NS_RSM).build();
        last_el.append_text_node(last);
        set.append_child(last_el);
    }
    let mut count_el = Element::builder("count", NS_RSM).build();
    count_el.append_text_node(page.total.to_string());
    set.append_child(count_el);
    threads.append_child(set);

    threads
}

fn build_thread_entry(entry: &ThreadEntry) -> Element {
    let last_activity_iso =
        chrono::DateTime::<chrono::Utc>::from_timestamp(entry.last_activity_secs, 0)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    let mut t = Element::builder("thread", NS_THREADS)
        .attr(
            minidom::rxml::xml_ncname!("channel").to_owned(),
            entry.channel.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("thread-id").to_owned(),
            entry.thread_id.clone(),
        )
        .attr(
            minidom::rxml::xml_ncname!("last-stanza-id").to_owned(),
            entry.last_stanza_id.clone(),
        )
        .attr(
            minidom::rxml::xml_ncname!("last-activity").to_owned(),
            last_activity_iso,
        )
        .attr(
            minidom::rxml::xml_ncname!("unread").to_owned(),
            entry.unread.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("reply-count").to_owned(),
            entry.reply_count.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("has-unread").to_owned(),
            if entry.has_unread() { "true" } else { "false" },
        )
        .build();

    if let Some(ref author) = entry.root_author {
        let mut author_el = Element::builder("root-author", NS_THREADS).build();
        author_el.append_text_node(author.to_string());
        t.append_child(author_el);
    }
    if let Some(ref preview) = entry.preview {
        let mut preview_el = Element::builder("preview", NS_THREADS).build();
        preview_el.append_text_node(preview);
        t.append_child(preview_el);
    }
    if let Some(ref title) = entry.thread_title {
        let mut title_el = Element::builder("thread-title", NS_THREADS).build();
        title_el.append_text_node(title);
        t.append_child(title_el);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::iq::Iq;

    fn make_get_iq(payload: Element) -> Iq {
        Iq::Get {
            from: None,
            to: None,
            id: "test".into(),
            payload,
        }
    }

    #[test]
    fn parse_empty_query() {
        let payload = Element::builder("query", NS_THREADS).build();
        let iq = make_get_iq(payload);
        let q = parse_threads_query(&iq).expect("parses");
        assert_eq!(q.page_size, None);
        assert_eq!(q.after_cursor, None);
    }

    #[test]
    fn parse_query_with_rsm() {
        let xml = "<query xmlns='urn:waddle:threads:0'>\
                     <set xmlns='http://jabber.org/protocol/rsm'>\
                       <max>25</max>\
                       <after>CURSOR-1</after>\
                     </set>\
                   </query>";
        let payload: Element = xml.parse().expect("valid XML");
        let iq = make_get_iq(payload);
        let q = parse_threads_query(&iq).expect("parses");
        assert_eq!(q.page_size, Some(25));
        assert_eq!(q.after_cursor.as_deref(), Some("CURSOR-1"));
    }

    #[test]
    fn parse_rejects_non_get_iq() {
        let payload = Element::builder("query", NS_THREADS).build();
        let iq = Iq::Set {
            from: None,
            to: None,
            id: "x".into(),
            payload,
        };
        assert!(matches!(
            parse_threads_query(&iq),
            Err(ThreadsError::WrongIqType)
        ));
    }

    #[test]
    fn build_empty_page() {
        let page = ThreadsPage::default();
        let elem = build_threads_response(&page);
        assert_eq!(elem.name(), "threads");
        assert_eq!(elem.ns(), NS_THREADS);
        assert_eq!(elem.attr("total"), Some("0"));
        assert_eq!(elem.attr("unread-threads"), Some("0"));
        assert!(elem
            .children()
            .any(|c| c.name() == "set" && c.ns() == NS_RSM));
    }

    #[test]
    fn build_single_entry_round_trip() {
        let entry = ThreadEntry {
            channel: "room@conference.example".parse().expect("valid bare JID"),
            thread_id: "t-1".into(),
            last_stanza_id: "S-1".into(),
            last_activity_secs: 1_700_000_000,
            unread: 2,
            reply_count: 5,
            root_author: Some("juliet@example.com".parse().expect("valid bare JID")),
            preview: Some("Anyone reviewed the doc?".into()),
            thread_title: Some("Q3 planning".into()),
        };
        let page = ThreadsPage {
            entries: vec![entry.clone()],
            total: 1,
            unread_threads: 1,
            first_cursor: Some("F".into()),
            last_cursor: Some("L".into()),
        };
        let elem = build_threads_response(&page);

        let thread_el = elem
            .children()
            .find(|c| c.name() == "thread")
            .expect("has thread");
        assert_eq!(thread_el.attr("channel"), Some("room@conference.example"));
        assert_eq!(thread_el.attr("thread-id"), Some("t-1"));
        assert_eq!(thread_el.attr("unread"), Some("2"));
        assert_eq!(thread_el.attr("has-unread"), Some("true"));
        assert_eq!(
            thread_el
                .get_child("root-author", NS_THREADS)
                .map(|e| e.text()),
            Some("juliet@example.com".into())
        );
    }

    #[test]
    fn has_unread_flag_is_false_when_unread_is_zero() {
        let entry = ThreadEntry {
            channel: "room@conference.example".parse().expect("valid bare JID"),
            thread_id: "t-1".into(),
            last_stanza_id: "S-1".into(),
            last_activity_secs: 1_700_000_000,
            unread: 0,
            reply_count: 1,
            root_author: None,
            preview: None,
            thread_title: None,
        };
        let page = ThreadsPage {
            entries: vec![entry],
            total: 1,
            unread_threads: 0,
            first_cursor: None,
            last_cursor: None,
        };
        let elem = build_threads_response(&page);
        let thread_el = elem
            .children()
            .find(|c| c.name() == "thread")
            .expect("has thread");
        assert_eq!(thread_el.attr("has-unread"), Some("false"));
    }
}
