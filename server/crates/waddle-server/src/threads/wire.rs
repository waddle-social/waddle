//! XML wire shape for `urn:waddle:threads:0`. Built via `minidom::Element`
//! so no XML is ever concatenated as strings (CLAUDE.md hard rule).

use minidom::Element;
use waddle_xmpp::xep::{CallThreadKind, CallThreadMedia};
use xmpp_parsers::iq::Iq;

use super::query::{
    ThreadEntry, ThreadSort, ThreadStatusFilter, ThreadsError, ThreadsPage, ThreadsQuery,
    NS_THREADS,
};

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
    if let Some(status) = payload.attr("status") {
        q.status = ThreadStatusFilter::parse(status)?;
    }
    if let Some(sort) = payload.attr("sort") {
        q.sort = ThreadSort::parse(sort)?;
    }
    if let Some(active_since) = payload.attr("active-since") {
        let parsed = chrono::DateTime::parse_from_rfc3339(active_since)
            .map_err(|_| ThreadsError::InvalidTimestamp(active_since.to_string()))?;
        q.active_since_secs = Some(parsed.timestamp());
    }
    if let Some(channel) = payload.attr("channel") {
        q.channel = Some(
            channel
                .parse()
                .map_err(|_| ThreadsError::InvalidChannel(channel.to_string()))?,
        );
    }
    if let Some(search) = payload.attr("search") {
        let trimmed = search.trim();
        if !trimmed.is_empty() {
            q.search = Some(trimmed.to_string());
        }
    }
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
    if let (Some(kind), Some(media)) = (entry.call_thread_kind, entry.call_thread_media) {
        let call_el = Element::builder("call", NS_THREADS)
            .attr(
                minidom::rxml::xml_ncname!("kind").to_owned(),
                call_kind_token(kind),
            )
            .attr(
                minidom::rxml::xml_ncname!("media").to_owned(),
                call_media_tokens(media),
            )
            .build();
        t.append_child(call_el);
    }
    if let (Some(ended_at), Some(ref duration)) = (entry.call_ended_at, &entry.call_duration) {
        let call_ended_el = Element::builder("call-ended", NS_THREADS)
            .attr(
                minidom::rxml::xml_ncname!("ended").to_owned(),
                ended_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )
            .attr(
                minidom::rxml::xml_ncname!("duration").to_owned(),
                duration.as_str(),
            )
            .build();
        t.append_child(call_ended_el);
    }
    t
}

/// `kind` attribute token for the `<call>` child.
fn call_kind_token(kind: CallThreadKind) -> &'static str {
    match kind {
        CallThreadKind::Dm => "dm",
        CallThreadKind::Muc => "muc",
    }
}

/// Space-joined `media` tokens, `audio` before `video`, matching the
/// call-thread anchor marker convention.
fn call_media_tokens(media: CallThreadMedia) -> &'static str {
    match (media.audio, media.video) {
        (true, true) => "audio video",
        (true, false) => "audio",
        (false, true) => "video",
        (false, false) => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::query::ThreadRootAuthor;
    use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};
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
        assert_eq!(q.status, ThreadStatusFilter::All);
        assert_eq!(q.sort, ThreadSort::Recent);
        assert_eq!(q.active_since_secs, None);
        assert_eq!(q.channel, None);
        assert_eq!(q.search, None);
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
    fn parse_query_with_filters() {
        let xml = "<query xmlns='urn:waddle:threads:0' \
                          status='unread' \
                          active-since='2026-05-19T00:00:00Z' \
                          channel='chat@muc.waddle.chat' \
                          search=' notifications ' \
                          sort='replies'/>";
        let payload: Element = xml.parse().expect("valid XML");
        let iq = make_get_iq(payload);
        let q = parse_threads_query(&iq).expect("parses");
        assert_eq!(q.status, ThreadStatusFilter::Unread);
        assert_eq!(q.active_since_secs, Some(1_779_148_800));
        assert_eq!(
            q.channel.as_ref().map(ToString::to_string).as_deref(),
            Some("chat@muc.waddle.chat")
        );
        assert_eq!(q.search.as_deref(), Some("notifications"));
        assert_eq!(q.sort, ThreadSort::Replies);
    }

    #[test]
    fn parse_rejects_invalid_status() {
        let payload: Element = "<query xmlns='urn:waddle:threads:0' status='stale'/>"
            .parse()
            .expect("valid XML");
        let iq = make_get_iq(payload);
        assert!(matches!(
            parse_threads_query(&iq),
            Err(ThreadsError::InvalidStatus(value)) if value == "stale"
        ));
    }

    #[test]
    fn parse_rejects_invalid_sort() {
        let payload: Element = "<query xmlns='urn:waddle:threads:0' sort='hot'/>"
            .parse()
            .expect("valid XML");
        let iq = make_get_iq(payload);
        assert!(matches!(
            parse_threads_query(&iq),
            Err(ThreadsError::InvalidSort(value)) if value == "hot"
        ));
    }

    #[test]
    fn parse_rejects_invalid_active_since() {
        let payload: Element = "<query xmlns='urn:waddle:threads:0' active-since='not-a-date'/>"
            .parse()
            .expect("valid XML");
        let iq = make_get_iq(payload);
        assert!(matches!(
            parse_threads_query(&iq),
            Err(ThreadsError::InvalidTimestamp(value)) if value == "not-a-date"
        ));
    }

    #[test]
    fn parse_rejects_invalid_channel() {
        let payload: Element = "<query xmlns='urn:waddle:threads:0' channel='not a jid'/>"
            .parse()
            .expect("valid XML");
        let iq = make_get_iq(payload);
        assert!(matches!(
            parse_threads_query(&iq),
            Err(ThreadsError::InvalidChannel(value)) if value == "not a jid"
        ));
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
            root_author: ThreadRootAuthor::parse("juliet"),
            preview: Some("Anyone reviewed the doc?".into()),
            thread_title: Some("Q3 planning".into()),
            call_thread_kind: None,
            call_thread_media: None,
            call_ended_at: None,
            call_duration: None,
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
            Some("juliet".into())
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
            call_thread_kind: None,
            call_thread_media: None,
            call_ended_at: None,
            call_duration: None,
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

    fn call_thread_entry() -> ThreadEntry {
        ThreadEntry {
            channel: "room@conference.example".parse().expect("valid bare JID"),
            thread_id: "call-1".into(),
            last_stanza_id: "S-9".into(),
            last_activity_secs: 1_700_000_000,
            unread: 0,
            reply_count: 0,
            root_author: None,
            preview: None,
            thread_title: None,
            call_thread_kind: Some(CallThreadKind::Muc),
            call_thread_media: Some(CallThreadMedia::audio_video()),
            call_ended_at: None,
            call_duration: None,
        }
    }

    fn thread_child(entry: ThreadEntry) -> Element {
        let page = ThreadsPage {
            entries: vec![entry],
            total: 1,
            unread_threads: 0,
            first_cursor: None,
            last_cursor: None,
        };
        let elem = build_threads_response(&page);
        elem.children()
            .find(|c| c.name() == "thread")
            .cloned()
            .expect("has thread")
    }

    #[test]
    fn build_emits_call_child_for_call_thread() {
        let thread_el = thread_child(call_thread_entry());

        let call = thread_el
            .get_child("call", NS_THREADS)
            .expect("has call child");
        assert_eq!(call.attr("kind"), Some("muc"));
        assert_eq!(call.attr("media"), Some("audio video"));
        assert!(
            thread_el.get_child("call-ended", NS_THREADS).is_none(),
            "ongoing call must not emit <call-ended>"
        );
    }

    #[test]
    fn build_emits_call_ended_child_when_ended() {
        let mut entry = call_thread_entry();
        entry.call_thread_media = Some(CallThreadMedia::audio_only());
        entry.call_ended_at = Some(
            "2026-06-07T14:35:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("ended timestamp"),
        );
        entry.call_duration = Some(CallThreadDuration::parse("PT5M").expect("duration"));

        let thread_el = thread_child(entry);

        let call = thread_el
            .get_child("call", NS_THREADS)
            .expect("has call child");
        assert_eq!(call.attr("kind"), Some("muc"));
        assert_eq!(call.attr("media"), Some("audio"));

        let ended = thread_el
            .get_child("call-ended", NS_THREADS)
            .expect("has call-ended child");
        assert_eq!(ended.attr("ended"), Some("2026-06-07T14:35:00Z"));
        assert_eq!(ended.attr("duration"), Some("PT5M"));
    }

    #[test]
    fn build_omits_call_children_for_non_call_thread() {
        let entry = ThreadEntry {
            channel: "room@conference.example".parse().expect("valid bare JID"),
            thread_id: "plain".into(),
            last_stanza_id: "S-1".into(),
            last_activity_secs: 1_700_000_000,
            unread: 0,
            reply_count: 0,
            root_author: None,
            preview: None,
            thread_title: None,
            call_thread_kind: None,
            call_thread_media: None,
            call_ended_at: None,
            call_duration: None,
        };
        let thread_el = thread_child(entry);

        assert!(thread_el.get_child("call", NS_THREADS).is_none());
        assert!(thread_el.get_child("call-ended", NS_THREADS).is_none());
    }
}
