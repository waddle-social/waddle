//! Read-only iCalendar export for community xCal events.
//!
//! Waddle's source of truth stays the XMPP PubSub xCal node. This HTTP
//! surface exists only so external calendar clients can subscribe to a stable
//! `text/calendar` URL.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use tracing::warn;
use url::Url;
use uuid::Uuid;

use crate::db::actor::{DbExecute, DbQueryOne};
use crate::db::Value;
use waddle_xmpp::pubsub::{AccessModel, PubSubStorage, StoredItem};
use waddle_xmpp_core::calendar_subscription::is_loopback_url;
use waddle_xmpp_core::icalendar::calendar_text;
use waddle_xmpp_core::xcal::CalendarItem;

use super::auth::AuthState;

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "v2.";
const TOKEN_CONTEXT: &[u8] = b"waddle:calendar-feed:v2";
const ICS_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";
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

#[derive(Debug, Error)]
pub(crate) enum CalendarSubscriptionError {
    #[error("Calendar subscriptions require a local account and an open calendar")]
    Forbidden,
    #[error("Calendar subscription service is unavailable")]
    Unavailable,
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

    fn local_owner(&self, owner: &BareJid) -> bool {
        owner.node().is_some()
            && owner.domain() == self.auth_state.caps_server_domain.as_jid().domain()
    }

    pub(crate) async fn subscription_url(
        &self,
        owner: &BareJid,
    ) -> Result<Url, CalendarSubscriptionError> {
        if !self.local_owner(owner) {
            return Err(CalendarSubscriptionError::Forbidden);
        }
        let community = expected_community_jid(&self.auth_state.xmpp_domain)
            .map_err(|_| CalendarSubscriptionError::Unavailable)?;
        if self
            .pubsub_storage
            .get_node(&community, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
            .await
            .map_err(|_| CalendarSubscriptionError::Unavailable)?
            .is_some_and(|node| node.config.access_model != AccessModel::Open)
        {
            return Err(CalendarSubscriptionError::Forbidden);
        }
        let mut url = Url::parse(&self.auth_state.base_url)
            .map_err(|_| CalendarSubscriptionError::Unavailable)?;
        let secure_origin =
            url.scheme() == "https" || (url.scheme() == "http" && is_loopback_url(&url));
        if !secure_origin
            || !url.has_host()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CalendarSubscriptionError::Unavailable);
        }

        // One statement returns the existing subscription even when two pods
        // issue the first link concurrently. Revocation deletes this row.
        let row = self
            .auth_state
            .session_manager
            .actor_ref()
            .ask(DbQueryOne {
                sql: "INSERT INTO calendar_feed_subscriptions (owner_jid, subscription_id)
                  VALUES (?, ?) ON CONFLICT (owner_jid) DO UPDATE SET owner_jid = excluded.owner_jid
                  RETURNING subscription_id"
                    .to_owned(),
                params: vec![owner.as_str().into(), Uuid::new_v4().to_string().into()],
            })
            .await
            .map_err(|_| CalendarSubscriptionError::Unavailable)?;
        let Some(Value::Text(stored)) = row.as_ref().and_then(|row| row.first()) else {
            return Err(CalendarSubscriptionError::Unavailable);
        };
        let id = Uuid::parse_str(stored).map_err(|_| CalendarSubscriptionError::Unavailable)?;
        url.path_segments_mut()
            .map_err(|_| CalendarSubscriptionError::Unavailable)?
            .pop_if_empty()
            .extend([
                "api",
                "calendar",
                "community",
                &sign_feed_token(&self.token_key, id),
                "events.ics",
            ]);
        Ok(url)
    }

    pub(crate) async fn revoke_subscription(
        &self,
        owner: &BareJid,
    ) -> Result<(), CalendarSubscriptionError> {
        if !self.local_owner(owner) {
            return Err(CalendarSubscriptionError::Forbidden);
        }
        self.auth_state
            .session_manager
            .actor_ref()
            .ask(DbExecute {
                sql: "DELETE FROM calendar_feed_subscriptions WHERE owner_jid = ?".to_owned(),
                params: vec![owner.as_str().into()],
            })
            .await
            .map_err(|_| CalendarSubscriptionError::Unavailable)?;
        Ok(())
    }

    async fn accepts_token(&self, token: &str) -> Result<bool, CalendarSubscriptionError> {
        let Some(id) = verify_feed_token(&self.token_key, token) else {
            return Ok(false);
        };
        let row = self
            .auth_state
            .session_manager
            .actor_ref()
            .ask(DbQueryOne {
                sql: "SELECT owner_jid FROM calendar_feed_subscriptions WHERE subscription_id = ?"
                    .to_owned(),
                params: vec![id.to_string().into()],
            })
            .await
            .map_err(|_| CalendarSubscriptionError::Unavailable)?;
        let Some(Value::Text(owner)) = row.as_ref().and_then(|row| row.first()) else {
            return Ok(false);
        };
        Ok(owner
            .parse::<BareJid>()
            .is_ok_and(|owner| self.local_owner(&owner)))
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
    let session = match state
        .auth_state
        .session_manager
        .validate_session(&session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "invalid_session",
                "Invalid session",
            );
        }
    };

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

    let Ok(owner) = session.user_jid.parse::<BareJid>() else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_session",
            "Session has no valid account",
        );
    };
    let url = match state.subscription_url(&owner).await {
        Ok(url) => url.to_string(),
        Err(CalendarSubscriptionError::Forbidden) => {
            return json_error(
                StatusCode::FORBIDDEN,
                "calendar_unavailable",
                "Calendar feed unavailable",
            )
        }
        Err(CalendarSubscriptionError::Unavailable) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "calendar_unavailable",
                "Calendar feed unavailable",
            )
        }
    };
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
    match state.accepts_token(&token).await {
        Ok(true) => {}
        Ok(false) => return text_response(StatusCode::NOT_FOUND, "Not found"),
        Err(_) => {
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Calendar feed unavailable",
            )
        }
    }
    let community_jid = match expected_community_jid(&state.auth_state.xmpp_domain) {
        Ok(jid) => jid,
        Err(error) => {
            warn!(error = %error, "Calendar feed unavailable: invalid community service JID");
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Calendar feed unavailable",
            );
        }
    };

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
    // The ACL and items are separate storage reads. A node that became
    // private while the item query ran must not expose its new contents.
    if !state
        .pubsub_storage
        .get_node(community_jid, node)
        .await?
        .is_some_and(|meta| meta.config.access_model == AccessModel::Open)
    {
        return Ok(None);
    }
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
    calendar_text(
        items
            .iter()
            .map(|stored| (&stored.item, stored.published_at)),
    )
}

fn subscription_mac(key: &[u8], id: Uuid) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key");
    mac.update(TOKEN_CONTEXT);
    mac.update(&[0]);
    mac.update(id.as_bytes());
    mac
}

fn sign_feed_token(key: &[u8], id: Uuid) -> String {
    let mut payload = [0; 48];
    payload[..16].copy_from_slice(id.as_bytes());
    payload[16..].copy_from_slice(&subscription_mac(key, id).finalize().into_bytes());
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload))
}

fn verify_feed_token(key: &[u8], token: &str) -> Option<Uuid> {
    let mut payload = [0; 48];
    let encoded = token.strip_prefix(TOKEN_PREFIX)?;
    if encoded.len() != 64 || URL_SAFE_NO_PAD.decode_slice(encoded, &mut payload).ok()? != 48 {
        return None;
    }
    let id = Uuid::from_slice(&payload[..16]).ok()?;
    subscription_mac(key, id)
        .verify_slice(&payload[16..])
        .ok()?;
    Some(id)
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
    use waddle_xmpp_core::xcal::{Freq, Rrule, VEvent, Weekday};

    fn feed_secret() -> &'static [u8; 32] {
        static KEY: std::sync::LazyLock<[u8; 32]> = std::sync::LazyLock::new(rand::random);
        &KEY
    }

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
        let db_pool = DatabasePool::try_new(config, pool_config).await.unwrap();
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

    async fn issue_test_token(state: &CalendarFeedState) -> String {
        let owner = format!("alice@{}", state.auth_state.xmpp_domain);
        let session = Session::new(&owner, "alice", "alice");
        state
            .auth_state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();
        let url = state
            .subscription_url(&owner.parse().unwrap())
            .await
            .unwrap();
        url.path_segments().unwrap().nth_back(1).unwrap().to_owned()
    }

    #[test]
    fn feed_token_round_trips_and_rejects_tampering() {
        let id = Uuid::new_v4();
        let token = sign_feed_token(feed_secret(), id);
        assert_eq!(verify_feed_token(feed_secret(), &token), Some(id));

        let mut tampered = token.clone();
        tampered.push('x');
        assert!(verify_feed_token(feed_secret(), &tampered).is_none());
        let mut other_secret = *feed_secret();
        other_secret[0] ^= 1;
        assert!(verify_feed_token(&other_secret, &token).is_none());
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
            assert!(line.len() <= 75);
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
            feed_secret(),
        ));
        let token = issue_test_token(&state).await;
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
        let state = Arc::new(CalendarFeedState::new(
            auth_state.clone(),
            Arc::clone(&storage),
            feed_secret(),
        ));
        storage
            .update_node_config(
                &community,
                waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
                &NodeConfig::spaces_public(),
            )
            .await
            .unwrap();
        let token = issue_test_token(&state).await;
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
        assert!(matches!(
            state
                .subscription_url(&"alice@localhost".parse().unwrap())
                .await,
            Err(CalendarSubscriptionError::Forbidden)
        ));
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
            feed_secret(),
        ));
        let token = issue_test_token(&state).await;
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
            feed_secret(),
        ));
        let token = issue_test_token(&state).await;
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
            feed_secret(),
        ));
        let token = issue_test_token(&state).await;
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
            feed_secret(),
        ));
        let token = issue_test_token(&state).await;
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
            feed_secret(),
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
    async fn subscription_rejects_public_http_and_accepts_loopback_http() {
        let mut auth = test_auth_state().await;
        let owner = "alice@localhost".parse().unwrap();
        let session = Session::new("alice@localhost", "alice", "alice");
        auth.session_manager.create_session(&session).await.unwrap();
        for (base, allowed) in [
            ("http://server.example.com", false),
            ("http://10.0.0.1", false),
            ("http://localhost.example.com", false),
            ("http://localhost:8888", true),
            ("http://127.42.1.2:8888", true),
            ("http://[::1]:8888", true),
        ] {
            Arc::get_mut(&mut auth)
                .expect("unique fixture state")
                .base_url = base.to_owned();
            let state = CalendarFeedState::new(
                Arc::clone(&auth),
                Arc::new(InMemoryPubSubStorage::new()),
                feed_secret(),
            );
            match state.subscription_url(&owner).await {
                Ok(url) => assert!(allowed && url.as_str().starts_with(base), "{base}"),
                Err(err) => assert!(
                    !allowed && matches!(err, CalendarSubscriptionError::Unavailable),
                    "{base}"
                ),
            }
        }
    }

    async fn read_feed(state: &Arc<CalendarFeedState>, url: &Url) -> StatusCode {
        router(Arc::clone(state))
            .oneshot(
                Request::builder()
                    .uri(url.path())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn subscription_is_shared_by_http_and_xmpp_and_revoked_per_account() {
        let auth = test_auth_state().await;
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let first = Arc::new(CalendarFeedState::new(
            Arc::clone(&auth),
            Arc::clone(&storage),
            feed_secret(),
        ));
        let second = Arc::new(CalendarFeedState::new(
            Arc::clone(&auth),
            storage,
            feed_secret(),
        ));
        let a = Session::new("alice@localhost", "alice", "alice");
        let b = Session::new("bob@localhost", "bob", "bob");
        auth.session_manager.create_session(&a).await.unwrap();
        auth.session_manager.create_session(&b).await.unwrap();
        let alice = a.user_jid.parse().unwrap();
        let bob = b.user_jid.parse().unwrap();
        let (first_a, other_a, b_url) = tokio::join!(
            first.subscription_url(&alice),
            second.subscription_url(&alice),
            second.subscription_url(&bob)
        );
        let (a_url, b_url) = (first_a.unwrap(), b_url.unwrap());
        assert_eq!(other_a.unwrap(), a_url);
        assert_ne!(a_url, b_url);

        let response = router(Arc::clone(&second))
            .oneshot(
                Request::builder()
                    .uri("/api/calendar/community-feed-url?community_jid=community.localhost")
                    .header(header::HOST, "untrusted.example")
                    .header("x-forwarded-host", "untrusted.example")
                    .header(SESSION_ID_HEADER, a.id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["url"],
            a_url.as_str()
        );

        first.revoke_subscription(&alice).await.unwrap();
        first.revoke_subscription(&alice).await.unwrap();
        assert_eq!(read_feed(&second, &a_url).await, StatusCode::NOT_FOUND);
        assert_eq!(read_feed(&second, &b_url).await, StatusCode::OK);
        assert_eq!(second.subscription_url(&bob).await.unwrap(), b_url);
        let fresh = second.subscription_url(&alice).await.unwrap();
        assert_ne!(fresh, a_url);
        assert_eq!(read_feed(&first, &fresh).await, StatusCode::OK);

        let (issued, revoked) = tokio::join!(
            first.subscription_url(&alice),
            second.revoke_subscription(&alice)
        );
        revoked.unwrap();
        assert_eq!(read_feed(&first, &fresh).await, StatusCode::NOT_FOUND);
        let issued = issued.unwrap();
        assert_eq!(
            read_feed(&first, &issued).await,
            if issued == fresh {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            }
        );
        let old_format = Url::parse("https://server.example.com/api/calendar/community/v1.Y29tbXVuaXR5LmxvY2FsaG9zdA.invalid/events.ics").unwrap();
        assert_eq!(read_feed(&first, &old_format).await, StatusCode::NOT_FOUND);
        assert!(matches!(
            first
                .subscription_url(&"alice@external.example".parse().unwrap())
                .await,
            Err(CalendarSubscriptionError::Forbidden)
        ));
    }

    #[tokio::test]
    async fn deleting_and_recreating_the_account_does_not_revive_its_feed() {
        let auth = test_auth_state().await;
        let state = Arc::new(CalendarFeedState::new(
            Arc::clone(&auth),
            Arc::new(InMemoryPubSubStorage::new()),
            feed_secret(),
        ));
        let session = Session::new("alice@localhost", "alice", "alice");
        auth.session_manager.create_session(&session).await.unwrap();
        let alice = session.user_jid.parse().unwrap();
        let old = state.subscription_url(&alice).await.unwrap();
        auth.session_manager
            .actor_ref()
            .ask(DbExecute {
                sql: "DELETE FROM users WHERE jid = ?".into(),
                params: vec![alice.as_str().into()],
            })
            .await
            .unwrap();
        assert_eq!(read_feed(&state, &old).await, StatusCode::NOT_FOUND);
        let recreated = Session::new("alice@localhost", "alice", "alice");
        auth.session_manager
            .create_session(&recreated)
            .await
            .unwrap();
        let new = state.subscription_url(&alice).await.unwrap();
        assert_ne!(old, new);
        assert_eq!(read_feed(&state, &old).await, StatusCode::NOT_FOUND);
        assert_eq!(read_feed(&state, &new).await, StatusCode::OK);
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
            feed_secret(),
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
