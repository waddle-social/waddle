//! Read-only iCalendar export for community xCal events.
//!
//! Waddle's source of truth stays the XMPP PubSub xCal node. This HTTP
//! surface exists only so external calendar clients can subscribe to a stable
//! `text/calendar` URL.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{debug, warn};
use waddle_xmpp::pubsub::{AccessModel, PubSubStorage, StoredItem};
use waddle_xmpp_core::xcal::{CalendarDateValue, CalendarItem, Rrule, RruleEnd, VEvent};

use super::auth::AuthState;

type HmacSha256 = Hmac<Sha256>;

const TOKEN_VERSION: &str = "v1";
const TOKEN_CONTEXT: &[u8] = b"waddle:calendar-feed:v1";
const ICS_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";
const ICS_PRODUCT_ID: &str = "-//Waddle//Community xCal Events//EN";
const MAX_CONTENT_LINE_BYTES: usize = 75;
const CALENDAR_FEED_MAX_ITEMS: usize = 1_000;
const CALENDAR_FEED_RAW_ITEM_SCAN_LIMIT: u32 = 10_000;
const SESSION_ID_HEADER: &str = "x-waddle-session-id";
const NO_STORE: &str = "no-store";

#[derive(Clone)]
pub struct CalendarFeedState {
    auth_state: Arc<AuthState>,
    pubsub_storage: Arc<dyn PubSubStorage>,
    token_key: Arc<[u8]>,
}

impl CalendarFeedState {
    pub fn new(
        auth_state: Arc<AuthState>,
        pubsub_storage: Arc<dyn PubSubStorage>,
        token_key: impl AsRef<[u8]>,
    ) -> Self {
        Self {
            auth_state,
            pubsub_storage,
            token_key: Arc::from(token_key.as_ref()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct FeedUrlQuery {
    community_jid: String,
}

#[derive(Debug, Serialize)]
struct FeedUrlResponse {
    url: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
    message: String,
}

struct StoredCalendarItem {
    item: CalendarItem,
    published_at: DateTime<Utc>,
}

pub fn router(state: Arc<CalendarFeedState>) -> Router {
    Router::new()
        .route(
            "/api/calendar/community-feed-url",
            get(community_feed_url_handler),
        )
        .route(
            "/api/calendar/community/{token}/events.ics",
            get(community_feed_handler),
        )
        .with_state(state)
}

async fn community_feed_url_handler(
    State(state): State<Arc<CalendarFeedState>>,
    Query(query): Query<FeedUrlQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(session_id) =
        extract_session_header(&headers).or_else(|| extract_session_cookie(&headers))
    else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "missing_session",
            "missing session cookie",
        );
    };
    match state
        .auth_state
        .session_manager
        .validate_session(&session_id)
        .await
    {
        Ok(_) => {}
        Err(error) => {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "invalid_session",
                error.to_string(),
            );
        }
    }

    let requested = match query.community_jid.parse::<BareJid>() {
        Ok(jid) => jid,
        Err(error) => {
            return json_error(StatusCode::BAD_REQUEST, "invalid_jid", error.to_string());
        }
    };
    let expected = match expected_community_jid(&state.auth_state.xmpp_domain) {
        Ok(jid) => jid,
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "calendar_feed_unavailable",
                error,
            );
        }
    };
    if requested != expected {
        return json_error(
            StatusCode::FORBIDDEN,
            "unsupported_community",
            "calendar feeds are only available for this deployment's community service",
        );
    }

    let token = sign_feed_token(&state.token_key, &requested);
    let url = format!(
        "{}/api/calendar/community/{}/events.ics",
        state.auth_state.base_url.trim_end_matches('/'),
        token,
    );
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, NO_STORE)],
        Json(FeedUrlResponse { url }),
    )
        .into_response()
}

async fn community_feed_handler(
    State(state): State<Arc<CalendarFeedState>>,
    Path(token): Path<String>,
) -> Response {
    let community_jid = match verify_feed_token(&state.token_key, &token) {
        Ok(jid) => jid,
        Err(error) => {
            debug!(error = %error, "Rejected invalid calendar feed token");
            return text_response(StatusCode::NOT_FOUND, "Not found");
        }
    };
    let expected = match expected_community_jid(&state.auth_state.xmpp_domain) {
        Ok(jid) => jid,
        Err(error) => {
            warn!(error = %error, "Calendar feed unavailable: invalid community service JID");
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Calendar feed unavailable",
            );
        }
    };
    if community_jid != expected {
        return text_response(StatusCode::NOT_FOUND, "Not found");
    }

    let items = match load_calendar_items(&state, &community_jid).await {
        Ok(Some(items)) => items,
        Ok(None) => {
            return text_response(StatusCode::NOT_FOUND, "Not found");
        }
        Err(error) => {
            warn!(error = %error, "Failed to load calendar feed items");
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Calendar feed unavailable",
            );
        }
    };
    let body = build_icalendar(&items);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ICS_CONTENT_TYPE),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        body,
    )
        .into_response()
}

async fn load_calendar_items(
    state: &CalendarFeedState,
    community_jid: &BareJid,
) -> Result<Option<Vec<StoredCalendarItem>>, waddle_xmpp::XmppError> {
    let node = waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS;
    let Some(node_meta) = state.pubsub_storage.get_node(community_jid, node).await? else {
        return Ok(Some(Vec::new()));
    };
    if node_meta.config.access_model != AccessModel::Open {
        warn!(
            community = %community_jid,
            node,
            access_model = %node_meta.config.access_model,
            "Calendar feed disabled for non-open PubSub node"
        );
        return Ok(None);
    }
    let stored_items = state
        .pubsub_storage
        .get_items(
            community_jid,
            node,
            Some(CALENDAR_FEED_RAW_ITEM_SCAN_LIMIT),
            &[],
        )
        .await?;
    let mut items = parse_stored_calendar_items(&stored_items);
    if items.len() > CALENDAR_FEED_MAX_ITEMS {
        let drop_count = items.len() - CALENDAR_FEED_MAX_ITEMS;
        items.drain(0..drop_count);
    }
    Ok(Some(items))
}

fn parse_stored_calendar_items(stored_items: &[StoredItem]) -> Vec<StoredCalendarItem> {
    let mut items = Vec::new();
    for stored in stored_items {
        let Some(payload_xml) = stored.payload_xml.as_deref() else {
            continue;
        };
        let Ok(payload) = payload_xml.parse::<Element>() else {
            continue;
        };
        let Some(item) = waddle_xmpp_core::xcal::parse_vcalendar_item(&stored.id, &payload) else {
            continue;
        };
        if is_rsvp_calendar_item(&stored.id, &item) {
            continue;
        }
        if item.master.dtstart.is_none()
            && item
                .overrides
                .iter()
                .all(|event| event.dtstart.is_none() && event.recurrence_id.is_none())
        {
            continue;
        }
        items.push(StoredCalendarItem {
            item,
            published_at: stored.published_at,
        });
    }
    items
}

fn is_rsvp_calendar_item(item_id: &str, item: &CalendarItem) -> bool {
    let Some(master_uid) = parse_rsvp_master_uid(item_id) else {
        return false;
    };
    item.master.uid == master_uid
        && item.master.dtstart.is_none()
        && item.master.recurrence_id.is_none()
        && !item.master.attendees.is_empty()
}

fn parse_rsvp_master_uid(item_id: &str) -> Option<&str> {
    let (master_uid, localpart) = item_id.rsplit_once("-rsvp-")?;
    if master_uid.is_empty() || localpart.is_empty() {
        return None;
    }
    Some(master_uid)
}

fn build_icalendar(items: &[StoredCalendarItem]) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        property("PRODID", ICS_PRODUCT_ID),
        "CALSCALE:GREGORIAN".to_string(),
    ];

    for stored in items {
        lines.extend(event_lines(&stored.item.master, None, stored.published_at));
        for override_event in &stored.item.overrides {
            lines.extend(event_lines(
                override_event,
                Some(&stored.item.master),
                stored.published_at,
            ));
        }
    }

    lines.push("END:VCALENDAR".to_string());
    format!(
        "{}\r\n",
        lines
            .iter()
            .map(|line| fold_content_line(line))
            .collect::<Vec<_>>()
            .join("\r\n")
    )
}

fn event_lines(
    event: &VEvent,
    master: Option<&VEvent>,
    published_at: DateTime<Utc>,
) -> Vec<String> {
    let Some(dtstart) = event.dtstart.or(event.recurrence_id) else {
        return Vec::new();
    };
    let dtstamp = event
        .dtstamp
        .or_else(|| master.and_then(|event| event.dtstamp))
        .unwrap_or(published_at);
    let dtend = event
        .dtend
        .or_else(|| master.and_then(|master| inherited_end(master, dtstart)));
    let summary = if event.summary.is_empty() {
        master.map(|event| event.summary.as_str()).unwrap_or("")
    } else {
        event.summary.as_str()
    };
    let description = event
        .description
        .as_deref()
        .or_else(|| master.and_then(|event| event.description.as_deref()));
    let location = event
        .location
        .as_deref()
        .or_else(|| master.and_then(|event| event.location.as_deref()));
    let organizer = event
        .organizer
        .as_deref()
        .or_else(|| master.and_then(|event| event.organizer.as_deref()));

    let mut lines = vec![
        "BEGIN:VEVENT".to_string(),
        property("UID", &event.uid),
        date_time_property("DTSTAMP", dtstamp),
        calendar_date_property("DTSTART", dtstart),
    ];
    if let Some(dtend) = dtend {
        lines.push(calendar_date_property("DTEND", dtend));
    }
    if let Some(recurrence_id) = event.recurrence_id {
        lines.push(calendar_date_property("RECURRENCE-ID", recurrence_id));
    }
    lines.push(property("SUMMARY", summary));
    if let Some(description) = description {
        lines.push(property("DESCRIPTION", description));
    }
    if let Some(location) = location {
        lines.push(property("LOCATION", location));
    }
    if let Some(organizer) = organizer {
        lines.push(cal_address_property("ORGANIZER", organizer));
    }
    if let Some(rrule) = event.rrule.as_ref() {
        lines.push(rrule_property(rrule));
    }
    if !event.exdates.is_empty() {
        let mut exdates = event.exdates.clone();
        exdates.sort();
        lines.push(exdate_property(exdates));
    }
    lines.push("END:VEVENT".to_string());
    lines
}

fn inherited_end(master: &VEvent, dtstart: CalendarDateValue) -> Option<CalendarDateValue> {
    let master_start = master.dtstart?;
    let master_end = master.dtend?;
    match (master_start, master_end, dtstart) {
        (
            CalendarDateValue::DateTime(master_start),
            CalendarDateValue::DateTime(master_end),
            CalendarDateValue::DateTime(dtstart),
        ) => {
            let duration = master_end.signed_duration_since(master_start);
            if duration.num_milliseconds() < 0 {
                return None;
            }
            dtstart
                .checked_add_signed(duration)
                .map(CalendarDateValue::DateTime)
        }
        (
            CalendarDateValue::Date(master_start),
            CalendarDateValue::Date(master_end),
            CalendarDateValue::Date(dtstart),
        ) => {
            let duration = master_end.signed_duration_since(master_start);
            if duration.num_days() < 0 {
                return None;
            }
            dtstart
                .checked_add_signed(duration)
                .map(CalendarDateValue::Date)
        }
        _ => None,
    }
}

fn property(name: &str, value: &str) -> String {
    format!("{name}:{}", escape_text(value))
}

fn cal_address_property(name: &str, value: &str) -> String {
    let mut line = String::with_capacity(name.len() + value.len() + 1);
    line.push_str(name);
    line.push(':');
    for ch in value.chars() {
        if ch != '\r' && ch != '\n' {
            line.push(ch);
        }
    }
    line
}

fn date_time_property(name: &str, value: DateTime<Utc>) -> String {
    format!("{name}:{}", format_utc_date_time(value))
}

fn calendar_date_property(name: &str, value: CalendarDateValue) -> String {
    match value {
        CalendarDateValue::Date(date) => {
            format!("{name};VALUE=DATE:{}", date.format("%Y%m%d"))
        }
        CalendarDateValue::DateTime(date_time) => {
            format!("{name}:{}", format_utc_date_time(date_time))
        }
    }
}

fn exdate_property(values: Vec<CalendarDateValue>) -> String {
    let all_day = values
        .first()
        .is_some_and(|value| matches!(value, CalendarDateValue::Date(_)));
    let encoded = values
        .into_iter()
        .map(|value| match value {
            CalendarDateValue::Date(date) => date.format("%Y%m%d").to_string(),
            CalendarDateValue::DateTime(date_time) => format_utc_date_time(date_time),
        })
        .collect::<Vec<_>>()
        .join(",");
    if all_day {
        format!("EXDATE;VALUE=DATE:{encoded}")
    } else {
        format!("EXDATE:{encoded}")
    }
}

fn rrule_property(rrule: &Rrule) -> String {
    let mut parts = vec![format!("FREQ={}", rrule.freq.as_str())];
    if let Some(interval) = rrule.interval {
        parts.push(format!("INTERVAL={interval}"));
    }
    if !rrule.by_day.is_empty() {
        parts.push(format!(
            "BYDAY={}",
            rrule
                .by_day
                .iter()
                .map(|day| day.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if !rrule.by_month_day.is_empty() {
        parts.push(format!(
            "BYMONTHDAY={}",
            rrule
                .by_month_day
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    match rrule.end {
        Some(RruleEnd::Count(count)) => parts.push(format!("COUNT={count}")),
        Some(RruleEnd::Until(until)) => {
            parts.push(format!("UNTIL={}", format_utc_date_time(until)))
        }
        None => {}
    }
    format!("RRULE:{}", parts.join(";"))
}

fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            _ => out.push(ch),
        }
    }
    out
}

fn format_utc_date_time(value: DateTime<Utc>) -> String {
    value.format("%Y%m%dT%H%M%SZ").to_string()
}

fn fold_content_line(line: &str) -> String {
    let mut out = String::new();
    let mut current_bytes = 0;
    for ch in line.chars() {
        let char_bytes = ch.len_utf8();
        if current_bytes > 0 && current_bytes + char_bytes > MAX_CONTENT_LINE_BYTES {
            out.push_str("\r\n ");
            current_bytes = 1;
        }
        out.push(ch);
        current_bytes += char_bytes;
    }
    out
}

fn sign_feed_token(key: &[u8], community_jid: &BareJid) -> String {
    let community = community_jid.to_string();
    let community_b64 = URL_SAFE_NO_PAD.encode(community.as_bytes());
    let signature = sign_token_payload(key, community.as_bytes());
    format!(
        "{TOKEN_VERSION}.{community_b64}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    )
}

fn verify_feed_token(key: &[u8], token: &str) -> Result<BareJid, String> {
    let mut parts = token.split('.');
    let version = parts.next().ok_or_else(|| "missing version".to_string())?;
    let community_b64 = parts.next().ok_or_else(|| "missing payload".to_string())?;
    let signature_b64 = parts
        .next()
        .ok_or_else(|| "missing signature".to_string())?;
    if parts.next().is_some() {
        return Err("too many token parts".to_string());
    }
    if version != TOKEN_VERSION {
        return Err("unsupported token version".to_string());
    }

    let community_bytes = URL_SAFE_NO_PAD
        .decode(community_b64)
        .map_err(|error| format!("invalid payload encoding: {error}"))?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|error| format!("invalid signature encoding: {error}"))?;
    verify_token_signature(key, &community_bytes, &signature)?;
    let community = String::from_utf8(community_bytes)
        .map_err(|error| format!("invalid community utf8: {error}"))?;
    community
        .parse::<BareJid>()
        .map_err(|error| format!("invalid community jid: {error}"))
}

fn sign_token_payload(key: &[u8], community: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key");
    mac.update(TOKEN_CONTEXT);
    mac.update(&[0]);
    mac.update(community);
    mac.finalize().into_bytes().to_vec()
}

fn verify_token_signature(key: &[u8], community: &[u8], signature: &[u8]) -> Result<(), String> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key");
    mac.update(TOKEN_CONTEXT);
    mac.update(&[0]);
    mac.update(community);
    mac.verify_slice(signature)
        .map_err(|_| "invalid signature".to_string())
}

fn expected_community_jid(xmpp_domain: &str) -> Result<BareJid, String> {
    format!("community.{xmpp_domain}")
        .parse::<BareJid>()
        .map_err(|error| format!("invalid community service JID: {error}"))
}

fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie_header.split(';') {
        let trimmed = pair.trim();
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if name == "waddle_session" && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn extract_session_header(headers: &HeaderMap) -> Option<String> {
    let session_id = headers.get(SESSION_ID_HEADER)?.to_str().ok()?.trim();
    if session_id.is_empty() {
        return None;
    }
    Some(session_id.to_string())
}

fn json_error(status: StatusCode, error: &'static str, message: impl Into<String>) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, NO_STORE)],
        Json(ErrorResponse {
            error,
            message: message.into(),
        }),
    )
        .into_response()
}

fn text_response(status: StatusCode, body: &'static str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::config::ServerConfig;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::AppState;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::TimeZone;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use waddle_xmpp::pubsub::{InMemoryPubSubStorage, NodeConfig, PubSubItem};
    use waddle_xmpp_core::xcal::{Freq, Rrule, Weekday};

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid timestamp")
    }

    fn day(y: i32, mo: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, mo, d).expect("valid date")
    }

    async fn test_auth_state() -> Arc<AuthState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig;
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .unwrap();
        let mut server_config = ServerConfig::test_homeserver();
        server_config.base_url = "https://server.example.com".to_string();
        Arc::new(AuthState::new(
            Arc::new(AppState::new(Arc::new(db_pool))),
            &server_config,
            &url::Url::parse("wss://xmpp.example.com/ws").expect("test WebSocket URL"),
            Some(b"test-session-key"),
        ))
    }

    #[test]
    fn feed_token_round_trips_and_rejects_tampering() {
        let community: BareJid = "community.localhost".parse().unwrap();
        let token = sign_feed_token(b"secret", &community);
        assert_eq!(verify_feed_token(b"secret", &token).unwrap(), community);

        let mut tampered = token.clone();
        tampered.push('x');
        assert!(verify_feed_token(b"secret", &tampered).is_err());
        assert!(verify_feed_token(b"other-secret", &token).is_err());
    }

    #[test]
    fn icalendar_projection_preserves_recurrence_and_omits_attendees() {
        let item = CalendarItem::new(
            VEvent::new("evt-series", "Friday Game Night")
                .with_dtstamp(ts(2026, 6, 1, 12, 0))
                .with_dtstart(ts(2026, 6, 5, 19, 0))
                .with_dtend(ts(2026, 6, 5, 22, 0))
                .with_description("Bring snacks, drinks; and games\nSecond line")
                .with_location("HQ, Room A")
                .with_organizer("MAILTO:u+rsvp@example.com;type=rsvp,alt")
                .with_rrule(
                    Rrule::new(Freq::Weekly)
                        .with_interval(2)
                        .with_by_day([Weekday::Friday])
                        .with_count(4),
                )
                .with_exdates([ts(2026, 6, 19, 19, 0), ts(2026, 6, 12, 19, 0)])
                .add_attendee(waddle_xmpp_core::xcal::Attendee::new(
                    "xmpp:bob@example.com",
                    waddle_xmpp_core::xcal::PartStat::Accepted,
                )),
        )
        .add_override(
            VEvent::new("evt-series", "")
                .with_recurrence_id(ts(2026, 6, 12, 19, 0))
                .with_dtstart(ts(2026, 6, 12, 20, 0)),
        );

        let ics = build_icalendar(&[StoredCalendarItem {
            item,
            published_at: ts(2026, 6, 1, 13, 0),
        }]);

        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(!ics.contains("METHOD:"));
        assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2);
        assert!(ics.contains("UID:evt-series\r\n"));
        assert!(ics.contains("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=FR;COUNT=4\r\n"));
        assert!(ics.contains("EXDATE:20260612T190000Z,20260619T190000Z\r\n"));
        assert!(ics.contains("RECURRENCE-ID:20260612T190000Z\r\n"));
        assert!(ics.contains("DTSTART:20260612T200000Z\r\n"));
        assert!(ics.contains("DESCRIPTION:Bring snacks\\, drinks\\; and games\\nSecond line\r\n"));
        assert!(ics.contains("ORGANIZER:MAILTO:u+rsvp@example.com;type=rsvp,alt\r\n"));
        assert!(!ics.contains("ORGANIZER:MAILTO:u+rsvp@example.com\\;type=rsvp\\,alt\r\n"));
        assert!(!ics.contains("ATTENDEE"));
        assert!(!ics.contains("bob@example.com"));
        for line in ics.trim_end().split("\r\n") {
            assert!(line.len() <= MAX_CONTENT_LINE_BYTES);
        }
    }

    #[test]
    fn icalendar_projection_exports_all_day_dates() {
        let item = CalendarItem::new(
            VEvent::new("evt-retreat", "Retreat")
                .with_dtstamp(ts(2026, 6, 1, 12, 0))
                .with_dtstart_date(day(2026, 6, 15))
                .with_dtend_date(day(2026, 6, 18))
                .with_exdate_dates([day(2026, 6, 22)]),
        );

        let ics = build_icalendar(&[StoredCalendarItem {
            item,
            published_at: ts(2026, 6, 1, 13, 0),
        }]);

        assert!(ics.contains("DTSTAMP:20260601T120000Z\r\n"));
        assert!(ics.contains("DTSTART;VALUE=DATE:20260615\r\n"));
        assert!(ics.contains("DTEND;VALUE=DATE:20260618\r\n"));
        assert!(ics.contains("EXDATE;VALUE=DATE:20260622\r\n"));
        assert!(!ics.contains("DTSTART:20260615T000000Z\r\n"));
    }

    #[tokio::test]
    async fn signed_feed_route_serves_pubsub_xcal_as_ics() {
        let auth_state = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        storage
            .get_or_create_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .unwrap();
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &NodeConfig::spaces_public(),
            )
            .await
            .unwrap();
        let payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
            &VEvent::new("evt-1", "Game Night")
                .with_dtstamp(ts(2026, 6, 1, 12, 0))
                .with_dtstart(ts(2026, 6, 5, 19, 0)),
        );
        storage
            .publish_item(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &PubSubItem {
                    id: Some("evt-1".to_string()),
                    publisher: None,
                    payload: Some(payload),
                },
                None,
                false,
            )
            .await
            .unwrap();

        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            storage,
            b"calendar-test-key",
        ));
        let token = sign_feed_token(b"calendar-test-key", &community);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/community/{token}/events.ics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            ICS_CONTENT_TYPE,
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            NO_STORE,
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ics = String::from_utf8(body.to_vec()).unwrap();
        assert!(ics.contains("SUMMARY:Game Night\r\n"));
        assert!(ics.contains("DTSTART:20260605T190000Z\r\n"));
    }

    #[tokio::test]
    async fn signed_feed_route_refuses_non_open_pubsub_node() {
        let auth_state = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        storage
            .get_or_create_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .unwrap();
        let mut config = NodeConfig::spaces_public();
        config.access_model = AccessModel::Whitelist;
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &config,
            )
            .await
            .unwrap();

        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            storage,
            b"calendar-test-key",
        ));
        let token = sign_feed_token(b"calendar-test-key", &community);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/community/{token}/events.ics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn signed_feed_route_caps_pubsub_items() {
        let auth_state = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        storage
            .get_or_create_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .unwrap();
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &NodeConfig::spaces_public(),
            )
            .await
            .unwrap();

        for index in 0..=CALENDAR_FEED_MAX_ITEMS {
            let summary = if index == 0 {
                "Oldest Marker".to_string()
            } else if index == CALENDAR_FEED_MAX_ITEMS {
                "Newest Marker".to_string()
            } else {
                format!("Event {index}")
            };
            let id = format!("evt-{index}");
            let payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
                &VEvent::new(&id, &summary).with_dtstart(ts(2026, 6, 5, 19, 0)),
            );
            storage
                .publish_item(
                    &community,
                    waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                    &PubSubItem {
                        id: Some(id),
                        publisher: None,
                        payload: Some(payload),
                    },
                    None,
                    false,
                )
                .await
                .unwrap();
        }

        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            storage,
            b"calendar-test-key",
        ));
        let token = sign_feed_token(b"calendar-test-key", &community);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/community/{token}/events.ics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ics = String::from_utf8(body.to_vec()).unwrap();
        assert!(!ics.contains("SUMMARY:Oldest Marker\r\n"));
        assert!(ics.contains("SUMMARY:Newest Marker\r\n"));
    }

    #[tokio::test]
    async fn signed_feed_route_caps_after_filtering_rsvp_items() {
        let auth_state = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        storage
            .get_or_create_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .unwrap();
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &NodeConfig::spaces_public(),
            )
            .await
            .unwrap();

        let master_uid = "evt-rsvp-heavy";
        let master_payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
            &VEvent::new(master_uid, "RSVP Heavy Event").with_dtstart(ts(2026, 6, 5, 19, 0)),
        );
        storage
            .publish_item(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &PubSubItem {
                    id: Some(master_uid.to_string()),
                    publisher: None,
                    payload: Some(master_payload),
                },
                None,
                false,
            )
            .await
            .unwrap();

        for index in 0..CALENDAR_FEED_MAX_ITEMS {
            let rsvp_payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
                &VEvent::new(master_uid, "").add_attendee(waddle_xmpp_core::xcal::Attendee::new(
                    format!("xmpp:user{index}@example.com"),
                    waddle_xmpp_core::xcal::PartStat::Accepted,
                )),
            );
            storage
                .publish_item(
                    &community,
                    waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                    &PubSubItem {
                        id: Some(format!("{master_uid}-rsvp-user-{index}")),
                        publisher: None,
                        payload: Some(rsvp_payload),
                    },
                    None,
                    false,
                )
                .await
                .unwrap();
        }

        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            storage,
            b"calendar-test-key",
        ));
        let token = sign_feed_token(b"calendar-test-key", &community);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/community/{token}/events.ics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ics = String::from_utf8(body.to_vec()).unwrap();
        assert!(ics.contains("SUMMARY:RSVP Heavy Event\r\n"));
        assert!(!ics.contains("user999@example.com"));
    }

    #[tokio::test]
    async fn signed_feed_route_honors_raw_scan_bound() {
        let auth_state = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        storage
            .get_or_create_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .unwrap();
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &NodeConfig::spaces_public(),
            )
            .await
            .unwrap();

        let master_uid = "evt-outside-raw-scan";
        let master_payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
            &VEvent::new(master_uid, "Outside Raw Scan").with_dtstart(ts(2026, 6, 5, 19, 0)),
        );
        storage
            .publish_item(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &PubSubItem {
                    id: Some(master_uid.to_string()),
                    publisher: None,
                    payload: Some(master_payload),
                },
                None,
                false,
            )
            .await
            .unwrap();

        for index in 0..CALENDAR_FEED_RAW_ITEM_SCAN_LIMIT {
            let rsvp_payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
                &VEvent::new(master_uid, "").add_attendee(waddle_xmpp_core::xcal::Attendee::new(
                    format!("xmpp:scan-user{index}@example.com"),
                    waddle_xmpp_core::xcal::PartStat::Accepted,
                )),
            );
            storage
                .publish_item(
                    &community,
                    waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                    &PubSubItem {
                        id: Some(format!("{master_uid}-rsvp-scan-user-{index}")),
                        publisher: None,
                        payload: Some(rsvp_payload),
                    },
                    None,
                    false,
                )
                .await
                .unwrap();
        }

        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            storage,
            b"calendar-test-key",
        ));
        let token = sign_feed_token(b"calendar-test-key", &community);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/community/{token}/events.ics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ics = String::from_utf8(body.to_vec()).unwrap();
        assert!(!ics.contains("SUMMARY:Outside Raw Scan\r\n"));
        assert!(!ics.contains("scan-user9999@example.com"));
    }

    #[tokio::test]
    async fn signed_feed_route_keeps_master_item_ids_containing_rsvp_separator() {
        let auth_state = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        storage
            .get_or_create_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .unwrap();
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &NodeConfig::spaces_public(),
            )
            .await
            .unwrap();
        let payload = waddle_xmpp_core::xcal::build_vcalendar_with_event(
            &VEvent::new("team-rsvp-retro", "RSVP Retrospective")
                .with_dtstamp(ts(2026, 6, 1, 12, 0))
                .with_dtstart(ts(2026, 6, 5, 19, 0)),
        );
        storage
            .publish_item(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &PubSubItem {
                    id: Some("team-rsvp-retro".to_string()),
                    publisher: None,
                    payload: Some(payload),
                },
                None,
                false,
            )
            .await
            .unwrap();

        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            storage,
            b"calendar-test-key",
        ));
        let token = sign_feed_token(b"calendar-test-key", &community);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/community/{token}/events.ics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ics = String::from_utf8(body.to_vec()).unwrap();
        assert!(ics.contains("UID:team-rsvp-retro\r\n"));
        assert!(ics.contains("SUMMARY:RSVP Retrospective\r\n"));
    }

    #[tokio::test]
    async fn feed_url_endpoint_requires_session_and_returns_signed_url() {
        let auth_state = test_auth_state().await;
        let session = Session::new("alice@localhost", "alice", "alice");
        auth_state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            Arc::new(InMemoryPubSubStorage::new()),
            b"calendar-test-key",
        ));

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/calendar/community-feed-url?community_jid={}",
                        community
                    ))
                    .header(header::COOKIE, format!("waddle_session={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            NO_STORE,
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let url = json["url"].as_str().unwrap();
        assert!(url.starts_with("https://server.example.com/api/calendar/community/"));
        assert!(url.ends_with("/events.ics"));
    }

    #[tokio::test]
    async fn feed_url_endpoint_accepts_fragment_session_id_header() {
        let auth_state = test_auth_state().await;
        let session = Session::new("alice@localhost", "alice", "alice");
        auth_state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();
        let community = expected_community_jid(&auth_state.xmpp_domain).unwrap();
        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            Arc::new(InMemoryPubSubStorage::new()),
            b"calendar-test-key",
        ));

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/calendar/community-feed-url?community_jid={}",
                        community
                    ))
                    .header(SESSION_ID_HEADER, session.id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
