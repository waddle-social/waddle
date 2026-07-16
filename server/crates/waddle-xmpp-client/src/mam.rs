//! MAM (XEP-0313) history queries for room and DM history.
//!
//! Exposes MAM parsers and page types for every build. With the `native`
//! feature enabled, also implements the MAM query helper on the native client
//! handle to send IQ queries, collect result stanzas, and return a structured
//! [`MamPage`] when the server signals completion via `<fin/>`.

use chrono::{DateTime, Utc};
use minidom::Element;
use waddle_xmpp_core::mam::{
    DELAY_NS, FORWARD_NS, FULLTEXT_MAM_FIELD, MAM_NS, RSM_NS, STANZA_ID_FILTER_FIELD,
    WADDLE_MAM_THREAD_FIELD,
};
use waddle_xmpp_core::xep0359::{OriginId, StanzaId as StableStanzaId, NS_SID as XEP0359_NS};

use std::collections::HashSet;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::time::Duration;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::sync::broadcast;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::time::timeout;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use uuid::Uuid;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::client::ClientHandle;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::error::{ClientError, ClientResult};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::event::ClientEvent;

const CLIENT_NS: &str = "jabber:client";
const DATA_FORMS_NS: &str = "jabber:x:data";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub const MAM_START_FIELD: &str = "start";
pub const MAM_END_FIELD: &str = "end";

/// A page of archived messages plus RSM pagination info.
#[derive(Debug, Clone)]
pub struct MamPage {
    pub messages: Vec<ArchivedMessage>,
    pub rsm: RsmPageInfo,
    pub query_id: String,
    /// True when the server sent `<fin complete='true'/>`.
    pub is_complete: bool,
}

/// One archived message unwrapped from a MAM result stanza.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    /// Inner forwarded `<message id='...'/>` value.
    pub id: Option<ArchivedMessageId>,
    pub stanza_id: Option<StableStanzaId>,
    /// XEP-0359 `<origin-id/>` from the inner forwarded message.
    pub origin_id: Option<OriginId>,
    pub timestamp: Option<DateTime<Utc>>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub body: Option<String>,
    pub thread: Option<String>,
    /// All valid XEP-0359 stanza IDs embedded in the inner message.
    pub stanza_ids: Vec<StableStanzaId>,
    /// XEP-0201 nested-thread parent. Populated from the `parent`
    /// attribute on the inner `<thread/>` element via the canonical
    /// `crate::xep::thread::parse_thread` helper. `None` for root
    /// threads or messages without a `<thread/>`.
    pub parent_thread_id: Option<String>,
    /// XEP-0045 MUC real JID from archived `<x><item jid='...'/></x>` payloads.
    pub author_real_jid: Option<String>,
    /// Raw inner `<message>` element for full parsing by the messaging module.
    pub inner: Element,
    /// Typed payload parsed once at the MAM boundary.
    pub payload: ArchivedPayload,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArchivedPayload {
    pub message: Option<Box<crate::messaging::InboundMessage>>,
    pub call: Option<Box<crate::messaging::InboundCallEvent>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchivedMessageId(String);

impl ArchivedMessageId {
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.is_empty() {
            None
        } else {
            Some(Self(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ArchivedMessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// RSM (XEP-0059) pagination info from a MAM `<fin/>`.
#[derive(Debug, Clone, Default)]
pub struct RsmPageInfo {
    pub first: Option<String>,
    pub last: Option<String>,
    pub count: Option<u32>,
    pub index: Option<u32>,
    pub is_complete: bool,
}

// ── Extension trait ──────────────────────────────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait MamExt {
    /// Fetch archived messages for a MUC room.
    fn fetch_room_history<'a>(
        &'a self,
        room_jid: &'a str,
        max: u32,
        before: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<MamPage>> + Send + 'a;

    /// Fetch archived messages for a 1:1 DM conversation.
    fn fetch_dm_history<'a>(
        &'a self,
        peer_jid: &'a str,
        max: u32,
        before: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<MamPage>> + Send + 'a;

    /// Full-text search of a MUC room archive via the XEP-0313 extended
    /// form field `{urn:xmpp:fulltext:0}fulltext` (Clark notation per
    /// XEP-0068). Returns the newest matching page first.
    fn search_room_history<'a>(
        &'a self,
        room_jid: &'a str,
        query: &'a str,
        max: u32,
    ) -> impl std::future::Future<Output = ClientResult<MamPage>> + Send + 'a;

    /// Full-text search of the account's personal archive, filtered to a
    /// 1:1 peer via the standard `with` field plus the
    /// `{urn:xmpp:fulltext:0}fulltext` extended form field. The archive
    /// address is the session's bound bare JID.
    fn search_dm_history<'a>(
        &'a self,
        peer_jid: &'a str,
        query: &'a str,
        max: u32,
    ) -> impl std::future::Future<Output = ClientResult<MamPage>> + Send + 'a;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl MamExt for ClientHandle {
    async fn fetch_room_history(
        &self,
        room_jid: &str,
        max: u32,
        before: Option<&str>,
    ) -> ClientResult<MamPage> {
        let query_id = Uuid::new_v4().to_string();
        let iq_id = Uuid::new_v4().to_string();

        let iq = build_mam_iq(&iq_id, &query_id, max, before, None, Some(room_jid));
        run_mam_query(self, iq, &query_id).await
    }

    async fn fetch_dm_history(
        &self,
        peer_jid: &str,
        max: u32,
        before: Option<&str>,
    ) -> ClientResult<MamPage> {
        let query_id = Uuid::new_v4().to_string();
        let iq_id = Uuid::new_v4().to_string();

        let iq = build_mam_iq(&iq_id, &query_id, max, before, Some(peer_jid), None);
        run_mam_query(self, iq, &query_id).await
    }

    async fn search_room_history(
        &self,
        room_jid: &str,
        query: &str,
        max: u32,
    ) -> ClientResult<MamPage> {
        let query_id = Uuid::new_v4().to_string();
        let iq_id = Uuid::new_v4().to_string();

        let iq = build_room_search_history_iq(&iq_id, &query_id, max, room_jid, query);
        run_mam_query(self, iq, &query_id).await
    }

    async fn search_dm_history(
        &self,
        peer_jid: &str,
        query: &str,
        max: u32,
    ) -> ClientResult<MamPage> {
        // The personal archive lives at the account's bare JID: address the
        // query with the session's bound JID (RFC 6120 resource binding),
        // mirroring how the WASM client targets its configured account JID.
        let account_jid = self
            .snapshot()
            .binding
            .map(|binding| binding.jid.to_bare())
            .ok_or(ClientError::Disconnected)?;
        let query_id = Uuid::new_v4().to_string();
        let iq_id = Uuid::new_v4().to_string();

        let iq = build_dm_search_history_iq(
            &iq_id,
            &query_id,
            max,
            account_jid.as_str(),
            peer_jid,
            query,
        );
        run_mam_query(self, iq, &query_id).await
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Builder for a MAM query IQ. Each `with_*` setter is chainable;
/// `build()` emits the final `<iq>` element.
pub struct MamIqBuilder<'a> {
    iq_id: &'a str,
    query_id: &'a str,
    max: u32,
    before: Option<&'a str>,
    after: Option<&'a str>,
    with_jid: Option<&'a str>,
    to_jid: Option<&'a str>,
    thread_id: Option<&'a str>,
    fulltext: Option<&'a str>,
    start: Option<&'a str>,
    end: Option<&'a str>,
    stanza_ids: Option<&'a [&'a str]>,
}

impl<'a> MamIqBuilder<'a> {
    /// New builder with required fields. All other fields default to `None`.
    pub fn new(iq_id: &'a str, query_id: &'a str, max: u32) -> Self {
        Self {
            iq_id,
            query_id,
            max,
            before: None,
            after: None,
            with_jid: None,
            to_jid: None,
            thread_id: None,
            fulltext: None,
            start: None,
            end: None,
            stanza_ids: None,
        }
    }

    pub fn before(mut self, value: &'a str) -> Self {
        self.before = Some(value);
        self
    }

    pub fn after(mut self, value: &'a str) -> Self {
        self.after = Some(value);
        self
    }

    pub fn with_jid(mut self, value: &'a str) -> Self {
        self.with_jid = Some(value);
        self
    }

    pub fn to_jid(mut self, value: &'a str) -> Self {
        self.to_jid = Some(value);
        self
    }

    pub fn thread_id(mut self, value: &'a str) -> Self {
        self.thread_id = Some(value);
        self
    }

    pub fn fulltext(mut self, value: &'a str) -> Self {
        self.fulltext = Some(value);
        self
    }

    pub fn start(mut self, value: &'a str) -> Self {
        self.start = Some(value);
        self
    }

    pub fn end(mut self, value: &'a str) -> Self {
        self.end = Some(value);
        self
    }

    pub fn stanza_ids(mut self, value: &'a [&'a str]) -> Self {
        self.stanza_ids = Some(value);
        self
    }

    /// Build the `<iq>` element.
    pub fn build(self) -> Element {
        let mut rsm = Element::builder("set", RSM_NS).append(
            Element::builder("max", RSM_NS)
                .append(self.max.to_string())
                .build(),
        );
        if let Some(before) = self.before {
            rsm = rsm.append(Element::builder("before", RSM_NS).append(before).build());
        }
        if let Some(after) = self.after {
            rsm = rsm.append(Element::builder("after", RSM_NS).append(after).build());
        }

        let mut form = Element::builder("x", DATA_FORMS_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(
                Element::builder("field", DATA_FORMS_NS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                    .append(
                        Element::builder("value", DATA_FORMS_NS)
                            .append(MAM_NS)
                            .build(),
                    )
                    .build(),
            );
        if let Some(with_jid) = self.with_jid {
            form = form.append(build_form_field("with", with_jid));
        }
        if let Some(thread_id) = self.thread_id {
            form = form.append(build_form_field(WADDLE_MAM_THREAD_FIELD, thread_id));
        }
        if let Some(fulltext) = self.fulltext {
            form = form.append(build_form_field(FULLTEXT_MAM_FIELD, fulltext));
        }
        if let Some(start) = self.start {
            form = form.append(build_form_field(MAM_START_FIELD, start));
        }
        if let Some(end) = self.end {
            form = form.append(build_form_field(MAM_END_FIELD, end));
        }
        if let Some(stanza_ids) = self.stanza_ids {
            if !stanza_ids.is_empty() {
                let mut field = Element::builder("field", DATA_FORMS_NS)
                    .attr(
                        minidom::rxml::xml_ncname!("var").to_owned(),
                        STANZA_ID_FILTER_FIELD,
                    )
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "text-multi");
                for id in stanza_ids {
                    field =
                        field.append(Element::builder("value", DATA_FORMS_NS).append(*id).build());
                }
                form = form.append(field.build());
            }
        }

        let query = Element::builder("query", MAM_NS)
            .attr(
                minidom::rxml::xml_ncname!("queryid").to_owned(),
                self.query_id,
            )
            .append(form.build())
            .append(rsm.build())
            .build();

        let mut iq = Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), self.iq_id)
            .append(query);
        if let Some(to_jid) = self.to_jid {
            iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to_jid);
        }
        iq.build()
    }
}

/// Build a MAM query IQ element.
///
/// * `with_jid` — set as the `<with>` data form field (DM queries).
/// * `to_jid`   — set as the `to` attribute on the IQ (room queries).
pub fn build_mam_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    before: Option<&str>,
    with_jid: Option<&str>,
    to_jid: Option<&str>,
) -> Element {
    let mut builder = MamIqBuilder::new(iq_id, query_id, max).before(before.unwrap_or(""));
    if let Some(value) = with_jid {
        builder = builder.with_jid(value);
    }
    if let Some(value) = to_jid {
        builder = builder.to_jid(value);
    }
    builder.build()
}

/// Build a full-text room-archive search IQ: targets the room archive
/// (`to=room`) with the `{urn:xmpp:fulltext:0}fulltext` extended form field
/// and an empty `<before/>` so the server pages backward from the newest
/// match (XEP-0059 §2.5 / XEP-0313 §4.3).
pub fn build_room_search_history_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    room_jid: &str,
    query: &str,
) -> Element {
    MamIqBuilder::new(iq_id, query_id, max)
        .before("")
        .to_jid(room_jid)
        .fulltext(query)
        .build()
}

/// Build a full-text DM search IQ against the account's personal archive
/// (`to=account`), constrained to the peer via the standard `with` field
/// plus the `{urn:xmpp:fulltext:0}fulltext` extended form field. An empty
/// `<before/>` requests the newest matching page first.
pub fn build_dm_search_history_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    account_jid: &str,
    peer_jid: &str,
    query: &str,
) -> Element {
    MamIqBuilder::new(iq_id, query_id, max)
        .before("")
        .to_jid(account_jid)
        .with_jid(peer_jid)
        .fulltext(query)
        .build()
}

fn build_form_field(var: &str, value: &str) -> Element {
    Element::builder("field", DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", DATA_FORMS_NS)
                .append(value)
                .build(),
        )
        .build()
}

fn bare_jid(value: &str) -> &str {
    value.split('/').next().unwrap_or(value)
}

fn parse_archived_author_real_jid(inner: &Element) -> Option<String> {
    inner
        .get_child("x", NS_MUC_USER)
        .and_then(|payload| payload.get_child("item", NS_MUC_USER))
        .and_then(|item| item.attr("jid"))
        .map(|jid| bare_jid(jid).to_string())
}

/// Accumulates MAM results for one query: filters out results belonging to
/// other queries and drops duplicates by `mam_id` (XEP-0198 §5 leaves weeding
/// out resume-replayed duplicates to the recipient). Every collection path —
/// both arms of the native [`run_mam_query`] and the WASM driver's pending
/// query — delegates to one instance per query, so the dedup state cannot
/// diverge between them.
pub struct MamResultCollector {
    query_id: String,
    seen_mam_ids: HashSet<String>,
    messages: Vec<ArchivedMessage>,
}

impl MamResultCollector {
    pub fn new(query_id: &str) -> Self {
        Self {
            query_id: query_id.to_string(),
            seen_mam_ids: HashSet::new(),
            messages: Vec::new(),
        }
    }

    pub fn query_id(&self) -> &str {
        &self.query_id
    }

    /// Keep the result if it belongs to this query and its `mam_id` has not
    /// been collected yet; ignore it otherwise.
    pub fn collect(&mut self, archived: ArchivedMessage) {
        if archived.query_id.as_deref() == Some(&self.query_id)
            && self.seen_mam_ids.insert(archived.mam_id.clone())
        {
            self.messages.push(archived);
        }
    }

    pub fn into_messages(self) -> Vec<ArchivedMessage> {
        self.messages
    }
}

/// Subscribe to the event bus, send the IQ, collect MAM result messages until
/// the IQ correlation resolves, then assemble and return a [`MamPage`].
///
/// The collector runs concurrently with `send_iq` via `tokio::select!`: MAM
/// `<message>` result stanzas arrive on the broadcast bus while the IQ's
/// `<fin/>` response resolves the `send_iq` oneshot. Once the oneshot resolves,
/// the `select!` exits the main loop and we drain any residual results still
/// buffered in the receiver — the driver dispatches transport events in order,
/// so by the time `<fin/>` reaches us every prior MAM result is already in our
/// receiver's ring.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
async fn run_mam_query(
    handle: &ClientHandle,
    iq: Element,
    query_id: &str,
) -> ClientResult<MamPage> {
    // Subscribe BEFORE sending so we don't miss any result messages.
    let mut rx = handle.events();

    let query = async {
        let mut collector = MamResultCollector::new(query_id);
        let mut send_iq_fut = Box::pin(handle.send_iq(iq));

        let fin_el = loop {
            tokio::select! {
                result = &mut send_iq_fut => break result?,
                event = rx.recv() => match event {
                    Ok(ClientEvent::MamResult(archived)) => collector.collect(*archived),
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(ClientError::Disconnected);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        };

        // Drain any MAM results that landed in the buffer between the final
        // `broadcast.send` and `send_iq` winning the select. Sequential
        // driver dispatch guarantees every prior MAM result is buffered
        // before the IQ result resolves the oneshot.
        loop {
            match rx.try_recv() {
                Ok(ClientEvent::MamResult(archived)) => collector.collect(*archived),
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Empty)
                | Err(broadcast::error::TryRecvError::Closed) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }

        Ok::<_, ClientError>((fin_el, collector.into_messages()))
    };

    let (fin_el, messages) = timeout(Duration::from_secs(30), query)
        .await
        .map_err(|_| ClientError::Disconnected)??;

    let (rsm, is_complete) = parse_fin_from_iq_result(&fin_el);

    Ok(MamPage {
        messages,
        rsm,
        query_id: query_id.to_string(),
        is_complete,
    })
}

/// Extract RSM and completeness from the IQ result element wrapping `<fin/>`.
pub fn parse_fin_from_iq_result(iq_result: &Element) -> (RsmPageInfo, bool) {
    // The result may be the raw <fin/> (if send_iq returns it directly) or
    // an <iq type='result'> wrapping a <fin/> child.
    let fin = if iq_result.name() == "fin" && iq_result.ns() == MAM_NS {
        Some(iq_result)
    } else {
        iq_result.get_child("fin", MAM_NS)
    };

    match fin.and_then(parse_mam_fin) {
        Some((rsm, complete)) => (rsm, complete),
        None => (RsmPageInfo::default(), false),
    }
}

// ── Public parse helpers ─────────────────────────────────────────────────────

/// Parse a MAM result `<message>` wrapper into an [`ArchivedMessage`].
///
/// Returns `None` if the element is not a MAM result message.
pub fn parse_mam_result(element: &Element) -> Option<ArchivedMessage> {
    if element.name() != "message" {
        return None;
    }

    let result = element.get_child("result", MAM_NS)?;

    let mam_id = result.attr("id")?.to_string();
    let query_id = result.attr("queryid").map(str::to_string);

    let forwarded = result.get_child("forwarded", FORWARD_NS)?;

    let delay = forwarded.get_child("delay", DELAY_NS);
    let timestamp = delay
        .and_then(|d| d.attr("stamp"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let inner = forwarded
        .children()
        .find(|c| c.name() == "message")?
        .clone();

    let from = inner.attr("from").map(str::to_string);
    let to = inner.attr("to").map(str::to_string);
    let message_type = inner.attr("type").unwrap_or("normal").to_string();
    let id = inner.attr("id").and_then(ArchivedMessageId::new);

    let body = inner
        .get_child("body", CLIENT_NS)
        .or_else(|| inner.get_child("body", ""))
        .map(|b| b.text());

    // XEP-0201: parse via the canonical `crate::xep::thread::parse_thread`
    // helper so the optional `parent` attribute is surfaced as a typed
    // field instead of being recoverable only via re-parsing `inner`
    // downstream of FFI.
    let thread_ref = crate::xep::thread::parse_thread(&inner);
    let thread = thread_ref.as_ref().map(|t| t.id.clone());
    let parent_thread_id = thread_ref.as_ref().and_then(|t| t.parent.clone());

    // XEP-0359 stanza-id embedded in the inner message.
    let stanza_ids: Vec<StableStanzaId> = inner
        .children()
        .filter(|child| child.name() == "stanza-id" && child.ns() == XEP0359_NS)
        .filter_map(parse_archived_stanza_id)
        .collect();
    let stanza_id = stanza_ids.first().cloned();
    let origin_id = inner
        .get_child("origin-id", XEP0359_NS)
        .and_then(|s| s.attr("id"))
        .filter(|id| !id.is_empty())
        .map(OriginId::new);
    let author_real_jid = parse_archived_author_real_jid(&inner);

    let payload = parse_archived_payload(&inner);

    Some(ArchivedMessage {
        mam_id,
        query_id,
        id,
        stanza_id,
        origin_id,
        timestamp,
        from,
        to,
        message_type,
        body,
        thread,
        stanza_ids,
        parent_thread_id,
        author_real_jid,
        inner,
        payload,
    })
}

fn parse_archived_payload(inner: &Element) -> ArchivedPayload {
    let message = match crate::messaging::parse(inner) {
        Some(crate::messaging::MessagingEvent::Message(message)) => Some(message),
        _ => None,
    };
    let call = crate::messaging::parse_call_event(inner).map(Box::new);
    ArchivedPayload { message, call }
}

fn parse_archived_stanza_id(element: &Element) -> Option<StableStanzaId> {
    let id = element.attr("id").filter(|id| !id.is_empty())?;
    let by = element
        .attr("by")
        .filter(|by| !by.is_empty())?
        .parse::<jid::Jid>()
        .ok()?;
    Some(StableStanzaId::new(id, by))
}

/// Parse a MAM `<fin/>` element into [`RsmPageInfo`] and a completeness flag.
///
/// Returns `None` if the element is not a MAM `<fin/>`.
pub fn parse_mam_fin(element: &Element) -> Option<(RsmPageInfo, bool)> {
    if element.name() != "fin" || element.ns() != MAM_NS {
        return None;
    }

    let is_complete = element.attr("complete") == Some("true");

    let set = element.get_child("set", RSM_NS);

    let first = set
        .and_then(|s| s.get_child("first", RSM_NS))
        .map(|e| e.text());

    let last = set
        .and_then(|s| s.get_child("last", RSM_NS))
        .map(|e| e.text());

    let count = set
        .and_then(|s| s.get_child("count", RSM_NS))
        .and_then(|e| e.text().parse::<u32>().ok());

    let index = set
        .and_then(|s| s.get_child("first", RSM_NS))
        .and_then(|e| e.attr("index"))
        .and_then(|v| v.parse::<u32>().ok());

    Some((
        RsmPageInfo {
            first,
            last,
            count,
            index,
            is_complete,
        },
        is_complete,
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
