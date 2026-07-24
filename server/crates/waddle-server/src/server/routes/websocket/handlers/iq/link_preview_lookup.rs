//! Waddle plaintext link-preview composer lookup.
//!
//! The lookup resolves OpenGraph metadata through the bounded HTTPS resolver,
//! then mints a scoped private token for the send-time XEP-0511 payload.
//!
//! Resolution runs OFF the per-connection frame dispatch path (#1470). The
//! dispatch-side handler validates and authorizes the request synchronously,
//! then spawns the resolver work and returns no frames, so the strictly
//! serial frame loop (RFC 6120 §10.1) moves on to the next stanza
//! immediately. The IQ result — still exactly one reply per request,
//! matched by id (RFC 6120 §8.2.3) — is delivered later through the
//! authoritative `UserActor` delivery seam as a server-generated
//! `DirectFrame`, which the destination records in the XEP-0198 unacked
//! queue like every other server-generated reply.
//! Before this, a cold-cache resolver round-trip stalled every stanza queued
//! behind the lookup (production evidence on #1470: 1.3 s dispatch stalls vs
//! a 13.6 ms dispatch p95).

use super::*;
use chrono::{Duration, SecondsFormat, Utc};
use kameo::actor::ActorRef;
use minidom::rxml::xml_ncname;
use tokio::sync::OwnedSemaphorePermit;
use tracing::Instrument;
use url::Url;
use waddle_xmpp::registry::UserRegistryActor;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

use crate::server::routes::interpret::{deliver_direct_to_full, FullJidDeliveryOutcome};
use waddle_xmpp::xep::{
    encode_link_preview_token_checked, LinkPreviewTokenData, LinkPreviewTokenImage,
    LinkPreviewTokenNativeVideo, LinkPreviewTokenPlayer, LinkPreviewTokenVideo,
    NS_WADDLE_LINK_PREVIEW,
};
#[cfg(test)]
use waddle_xmpp_core::PreviewImageMediaType;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

use super::link_preview_resolver::{
    classify_url_with_policy, resolve_link_preview, LinkPreviewMediaCache,
    LinkPreviewResolverOutcome, LinkPreviewResolverPolicy, LinkPreviewResolverStatus,
};
use crate::server::routes::websocket::link_preview_telemetry::{
    record_link_preview_event, LinkPreviewTelemetryEvent,
};

struct LinkPreviewLookupDeps<'a> {
    muc_domain: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
    secret: &'a [u8],
    resolver_policy: Option<LinkPreviewResolverPolicy>,
}

pub(super) fn is_link_preview_lookup_iq(iq: &Iq) -> bool {
    let Iq::Get { payload, .. } = iq else {
        return false;
    };
    payload.name() == "lookup" && payload.ns() == NS_WADDLE_LINK_PREVIEW
}

pub(super) async fn handle_link_preview_lookup_iq(
    iq: &Iq,
    sender_jid: Option<&FullJid>,
    state: &WebSocketState,
    muc_domain: &str,
    response_from: Option<&str>,
    response_to: Option<&str>,
    secret: &[u8],
) -> Vec<String> {
    handle_link_preview_lookup_iq_with_policy(
        iq,
        sender_jid,
        state,
        LinkPreviewLookupDeps {
            muc_domain,
            response_from,
            response_to,
            secret,
            resolver_policy: None,
        },
    )
    .await
}

async fn handle_link_preview_lookup_iq_with_policy(
    iq: &Iq,
    sender_jid: Option<&FullJid>,
    state: &WebSocketState,
    deps: LinkPreviewLookupDeps<'_>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let resolver_policy = match deps.resolver_policy {
        Some(policy) => policy,
        None => LinkPreviewResolverPolicy::from_config(
            &state.deps.link_preview,
            Some(LinkPreviewMediaCache::new(
                state.deps.app_state.blob_storage.clone(),
                state.deps.auth_state.base_url.as_str(),
                state.deps.app_state.db_pool.global_actor().clone(),
                sender_jid.to_bare(),
            )),
        ),
    };
    let Iq::Get { payload, .. } = iq else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            bad_request_iq_error("Link preview lookup must be an IQ get."),
        )];
    };

    let Some(original_url) = lookup_url(payload) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            bad_request_iq_error("Link preview lookup requires a URL."),
        )];
    };
    let Some(scope_jid) = lookup_scope(payload).and_then(|scope| scope.parse().ok()) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            bad_request_iq_error("Link preview lookup requires a conversation scope JID."),
        )];
    };
    if let Some(error) =
        authorize_link_preview_scope(state, sender_jid, &scope_jid, deps.muc_domain).await
    {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            error,
        )];
    }

    let Ok(original_url) = Url::parse(&original_url) else {
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            LinkPreviewResolverStatus::Unsupported,
            None,
        )];
    };
    if !resolver_policy.enabled {
        record_link_preview_event(LinkPreviewTelemetryEvent::ResolverBlocked);
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            LinkPreviewResolverStatus::Blocked,
            None,
        )];
    }
    // Deterministic pre-I/O policy verdicts (scheme support, IP literals,
    // host allow/block lists) stay on the dispatch path: they need no
    // network and no resolve permit, and under saturation they must keep
    // answering `blocked`/`unsupported` rather than degrading to `failed`.
    let pre_verdict = classify_url_with_policy(&original_url, &resolver_policy);
    if !matches!(pre_verdict, LinkPreviewResolverStatus::Ready) {
        record_link_preview_event(telemetry_event_for_status(pre_verdict));
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            pre_verdict,
            None,
        )];
    }
    // Bounded admission (#1470): `try_acquire` against the per-node resolver
    // semaphore — a saturated resolver answers `failed` immediately
    // (previews fail open, #822) rather than queueing tasks whose replies
    // would outlive the client's IQ budget and whose spans would stay open
    // for the whole queue wait (#1438).
    let Ok(resolve_permit) = state
        .deps
        .protocol
        .link_preview_resolves
        .clone()
        .try_acquire_owned()
    else {
        record_link_preview_event(LinkPreviewTelemetryEvent::ResolverSaturated);
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            LinkPreviewResolverStatus::Failed,
            None,
        )];
    };
    // Accepted for resolution. Everything from here involves outbound
    // network fetches, so it runs off the dispatch path (#1470): spawn the
    // resolve and answer the IQ later through the connection registry.
    spawn_deferred_lookup_resolution(
        resolve_permit,
        DeferredLookupResolution {
            user_registry: state.deps.protocol.user_registry.clone(),
            sm_session_registry: state.deps.protocol.sm_session_registry.clone(),
            requester: sender_jid.clone(),
            scope_jid,
            original_url,
            resolver_policy,
            secret: LinkPreviewSigningSecret(deps.secret.to_vec()),
            reply: DeferredLookupReply {
                id: iq.id().to_string(),
                from: deps
                    .response_from
                    .and_then(|value| value.parse::<Jid>().ok()),
                to: deps.response_to.and_then(|value| value.parse::<Jid>().ok()),
            },
        },
    );
    Vec::new()
}

/// Signing key for composer preview tokens, typed so the deferred callback
/// envelope never carries raw secret bytes as an untyped blob
/// (typed-payloads rule). Today this wraps the deployment occupant-id
/// secret's key material, which the lookup path has always reused for token
/// HMACs (see the `handle_iq_with_conn_state` call site).
struct LinkPreviewSigningSecret(Vec<u8>);

impl LinkPreviewSigningSecret {
    fn key(&self) -> &[u8] {
        &self.0
    }
}

/// Everything one deferred lookup resolution owns once dispatch has moved
/// on: registry handles for the eventual reply, the authorized request
/// parameters, and the reply envelope.
struct DeferredLookupResolution {
    /// Authoritative delivery seam (ADR-0017): the `UserActor` resolved
    /// through this registry owns the requester's live channel — including,
    /// under clustering, remote resources registered by the route bridge —
    /// so the deferred reply follows the same DirectFrame path as every
    /// other server-generated frame.
    user_registry: ActorRef<UserRegistryActor>,
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
    requester: FullJid,
    scope_jid: BareJid,
    original_url: Url,
    resolver_policy: LinkPreviewResolverPolicy,
    secret: LinkPreviewSigningSecret,
    reply: DeferredLookupReply,
}

/// Reply envelope captured from the request before dispatch returns: the
/// request id the result must echo plus RFC 6120 §8.2.3 addressing
/// (response `from` = request `to`, response `to` = request `from`).
struct DeferredLookupReply {
    id: String,
    from: Option<Jid>,
    to: Option<Jid>,
}

fn spawn_deferred_lookup_resolution(
    resolve_permit: OwnedSemaphorePermit,
    resolution: DeferredLookupResolution,
) {
    // Created while the dispatch span is current, so the resolve — and the
    // resolver's child `link_preview.fetch` spans — stay on the originating
    // stanza's trace after dispatch returns (#1438: bounded, parented spans).
    let span = tracing::info_span!("link_preview.resolve", host = tracing::field::Empty);
    if let Some(host) = resolution.original_url.host_str() {
        span.record("host", host);
    }
    tokio::spawn(
        async move {
            let DeferredLookupResolution {
                user_registry,
                sm_session_registry,
                requester,
                scope_jid,
                original_url,
                resolver_policy,
                secret,
                reply,
            } = resolution;
            let reply_iq = resolve_deferred_lookup(
                &requester,
                scope_jid,
                &original_url,
                &resolver_policy,
                secret.key(),
                reply,
            )
            .await;
            // Release the fetch-concurrency permit before delivery: the
            // delivery path below is bounded (non-blocking actor try-send
            // with capped ask timeouts), but it is still not fetch work —
            // a slow recipient must not pin a resolver slot its fetch is
            // no longer using.
            drop(resolve_permit);
            deliver_deferred_lookup_reply(
                &user_registry,
                &sm_session_registry,
                &requester,
                reply_iq,
            )
            .await;
        }
        .instrument(span),
    );
}

/// Run the bounded resolver and build the typed IQ result for the requester
/// — the deferred continuation of the dispatch-side handler above.
async fn resolve_deferred_lookup(
    requester: &FullJid,
    scope_jid: BareJid,
    original_url: &Url,
    resolver_policy: &LinkPreviewResolverPolicy,
    secret: &[u8],
    reply: DeferredLookupReply,
) -> Iq {
    let outcome = resolve_link_preview(original_url, resolver_policy).await;
    let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
        record_link_preview_event(telemetry_event_for_status(outcome.status()));
        return build_link_preview_lookup_result_iq(reply, outcome.status(), None);
    };
    let metadata = *metadata;
    let expires_at = Utc::now() + Duration::minutes(5);
    let data = LinkPreviewTokenData {
        sender_jid: requester.to_bare(),
        scope_jid,
        original_url: metadata.original_url,
        normalized_url: metadata.normalized_url,
        title: metadata.title,
        description: metadata.description,
        image: metadata.image.map(|image| LinkPreviewTokenImage {
            url: image.url,
            media_type: image.media_type,
            width: image.width,
            height: image.height,
            alt: image.alt,
        }),
        video: metadata.video.map(|video| LinkPreviewTokenVideo {
            url: video.url,
            media_type: video.media_type,
            size: video.size,
        }),
        native_video: metadata
            .native_video
            .map(|native| LinkPreviewTokenNativeVideo {
                url: native.url,
                media_type: native.media_type,
            }),
        player: metadata.player_embed.map(|player| LinkPreviewTokenPlayer {
            url: player.url,
            width: player.width,
            height: player.height,
        }),
        expires_at_unix: expires_at.timestamp(),
    };
    let Some(token) = encode_link_preview_token_checked(&data, secret) else {
        record_link_preview_event(LinkPreviewTelemetryEvent::ResolverFailed);
        return build_link_preview_lookup_result_iq(reply, LinkPreviewResolverStatus::Failed, None);
    };

    record_link_preview_event(LinkPreviewTelemetryEvent::ResolverReady);
    build_link_preview_lookup_result_iq(
        reply,
        LinkPreviewResolverStatus::Ready,
        Some((
            data,
            token.as_str(),
            expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        )),
    )
}

/// Deliver the deferred IQ result to the requester as a server-generated
/// `DirectFrame` through [`deliver_direct_to_full`] — the authoritative
/// `UserActor` delivery seam (ADR-0017). That helper is bounded end to end
/// (non-blocking `TrySendDirect` with capped ask timeouts and a finite
/// dropped-full retry schedule, never a channel-capacity wait), covers
/// clustered remote resources registered with the owner actor, and falls
/// back to the detached XEP-0198 replay buffer itself — preserving the old
/// synchronous path's replay-on-resume guarantee. The destination's main
/// loop records the frame in the XEP-0198 unacked queue before the wire
/// write, like every other server-generated reply.
///
/// Deliberately NOT owner-gated: an XEP-0198 resume onto a new connection
/// registers a fresh owner token, and the requester's pending IQ survives
/// that resume — owner-gating would drop the reply in exactly the race the
/// SM replay covers. The residual cost is a reply delivered to a
/// same-full-JID replacement session, which ignores an IQ result whose id
/// it never issued (RFC 6120 §8.2.3).
async fn deliver_deferred_lookup_reply(
    user_registry: &ActorRef<UserRegistryActor>,
    sm_session_registry: &Arc<InMemorySmSessionRegistry>,
    requester: &FullJid,
    reply_iq: Iq,
) {
    let stanza = Stanza::Iq(Box::new(reply_iq));
    // Bounded retry tail over the whole delivery attempt. It covers two
    // distinct transient failures with one loop:
    //
    // - `Unavailable`: a resume completed between the live attempt and the
    //   detached-record fallback (resume consumes the session) — the next
    //   attempt finds the freshly resumed connection.
    // - `Dropped`: a CONNECTED requester's outbound channel stayed full
    //   through `deliver_direct_to_full`'s short in-line retry schedule
    //   (e.g. an XEP-0198 send-window pause). The old synchronous path
    //   could not lose the reply here — it wrote on the connection's own
    //   loop — so give the channel a few bounded, spaced chances to drain
    //   before conceding.
    //
    // Retrying `Dropped` is safe for THIS stanza class even though it can
    // include maybe-enqueued ask failures: a duplicate IQ *result* is
    // ignored by id (RFC 6120 §8.2.3), unlike the groupchat reflections
    // whose #1263 semantics forbid exactly this kind of replay. Total added
    // wait is ~5.25 s — far below the client's ~30 s IQ budget — the task
    // count stays bounded by the admission semaphore's throughput, and the
    // resolver permit was released before delivery, so no fetch slot is
    // pinned while waiting.
    const DELIVERY_RETRY_DELAYS: [std::time::Duration; 3] = [
        std::time::Duration::from_millis(250),
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(4),
    ];
    let mut delays = DELIVERY_RETRY_DELAYS.iter();
    loop {
        match deliver_direct_to_full(
            Some(user_registry),
            Some(sm_session_registry),
            requester,
            &stanza,
        )
        .await
        {
            FullJidDeliveryOutcome::Delivered => return,
            FullJidDeliveryOutcome::QueuedDetached => {
                debug!(
                    requester = %requester,
                    "deferred link-preview reply recorded for detached SM session"
                );
                return;
            }
            #[cfg(feature = "clustering")]
            FullJidDeliveryOutcome::MaybeCommitted => return,
            outcome @ (FullJidDeliveryOutcome::Unavailable | FullJidDeliveryOutcome::Dropped) => {
                let Some(delay) = delays.next() else {
                    // The requester is either truly gone or unrecoverably
                    // backpressured; their own IQ timeout owns the failure
                    // (previews fail open, #822).
                    warn!(
                        requester = %requester,
                        ?outcome,
                        "deferred link-preview reply dropped after bounded delivery retries"
                    );
                    return;
                };
                tokio::time::sleep(*delay).await;
            }
        }
    }
}

fn telemetry_event_for_status(status: LinkPreviewResolverStatus) -> LinkPreviewTelemetryEvent {
    match status {
        LinkPreviewResolverStatus::Ready => LinkPreviewTelemetryEvent::ResolverReady,
        LinkPreviewResolverStatus::Blocked => LinkPreviewTelemetryEvent::ResolverBlocked,
        LinkPreviewResolverStatus::Failed => LinkPreviewTelemetryEvent::ResolverFailed,
        LinkPreviewResolverStatus::Unsupported => LinkPreviewTelemetryEvent::ResolverUnsupported,
    }
}

async fn authorize_link_preview_scope(
    state: &WebSocketState,
    sender_jid: &FullJid,
    scope_jid: &BareJid,
    muc_domain: &str,
) -> Option<xmpp_parsers::stanza_error::StanzaError> {
    if scope_jid.domain().as_str() != muc_domain {
        return None;
    }

    let Some(room_actor) = get_room_actor(state, scope_jid).await else {
        return Some(forbidden_iq_error(
            "Link preview lookup is visible only to current room occupants.",
        ));
    };
    match room_actor
        .ask(GetOccupantByJid {
            jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(_)) => None,
        Ok(None) => Some(forbidden_iq_error(
            "Link preview lookup is visible only to current room occupants.",
        )),
        Err(error) => {
            warn!(
                scope = %scope_jid,
                ?error,
                "Link preview lookup: failed to verify room occupancy"
            );
            Some(internal_server_error_iq_error("Internal server error."))
        }
    }
}

fn lookup_url(payload: &Element) -> Option<String> {
    payload
        .get_child("url", NS_WADDLE_LINK_PREVIEW)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn lookup_scope(payload: &Element) -> Option<String> {
    payload
        .get_child("scope", NS_WADDLE_LINK_PREVIEW)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Serialize a lookup result for the synchronous short-circuit paths
/// (malformed URL, resolver disabled) that still answer inline on the
/// dispatch path.
fn build_link_preview_lookup_result(
    iq: &Iq,
    response_from: Option<&str>,
    response_to: Option<&str>,
    status: LinkPreviewResolverStatus,
    preview: Option<(LinkPreviewTokenData, &str, String)>,
) -> String {
    iq_to_xml(build_link_preview_lookup_result_iq(
        DeferredLookupReply {
            id: iq.id().to_string(),
            from: response_from.and_then(|value| value.parse::<Jid>().ok()),
            to: response_to.and_then(|value| value.parse::<Jid>().ok()),
        },
        status,
        preview,
    ))
}

/// Build the typed lookup result IQ — the single wire-shape authority for
/// both the synchronous short-circuit replies and the deferred registry
/// delivery.
fn build_link_preview_lookup_result_iq(
    reply: DeferredLookupReply,
    status: LinkPreviewResolverStatus,
    preview: Option<(LinkPreviewTokenData, &str, String)>,
) -> Iq {
    let mut lookup = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW).attr(
        xml_ncname!("status").to_owned(),
        if preview.is_some() {
            "ready"
        } else {
            status.as_lookup_status()
        },
    );
    if let Some((data, token, expires_at)) = preview {
        let mut preview = Element::builder("preview", NS_WADDLE_LINK_PREVIEW)
            .attr(xml_ncname!("token").to_owned(), token)
            .attr(
                xml_ncname!("original-url").to_owned(),
                data.original_url.as_str(),
            )
            .attr(
                xml_ncname!("normalized-url").to_owned(),
                data.normalized_url.as_str(),
            )
            .attr(xml_ncname!("expires-at").to_owned(), expires_at)
            .build();
        append_text(&mut preview, "title", data.title.as_deref());
        append_text(&mut preview, "description", data.description.as_deref());
        if let Some(image) = &data.image {
            let mut image_elem = Element::builder("image", NS_WADDLE_LINK_PREVIEW)
                .attr(xml_ncname!("url").to_owned(), image.url.as_str())
                .attr(
                    xml_ncname!("media-type").to_owned(),
                    image.media_type.as_str(),
                );
            if let Some(width) = image.width {
                image_elem = image_elem.attr(xml_ncname!("width").to_owned(), width.to_string());
            }
            if let Some(height) = image.height {
                image_elem = image_elem.attr(xml_ncname!("height").to_owned(), height.to_string());
            }
            if let Some(alt) = image.alt.as_deref().filter(|alt| !alt.trim().is_empty()) {
                image_elem = image_elem.attr(xml_ncname!("alt").to_owned(), alt.trim());
            }
            preview.append_child(image_elem.build());
        }
        if let Some(video) = &data.video {
            let mut video_elem = Element::builder("video", NS_WADDLE_LINK_PREVIEW)
                .attr(xml_ncname!("url").to_owned(), video.url.as_str())
                .attr(
                    xml_ncname!("media-type").to_owned(),
                    video.media_type.as_str(),
                );
            if let Some(size) = video.size {
                video_elem = video_elem.attr(xml_ncname!("size").to_owned(), size.to_string());
            }
            preview.append_child(video_elem.build());
        }
        if let Some(native) = &data.native_video {
            // A page-advertised native stream surfaces as the same `<video>`
            // element a direct-media file uses (no size), keeping the lookup
            // result shape uniform for both native-video kinds. (The composer
            // preview currently renders only the image/text card — see
            // link-preview.ts; wiring a video card there is a follow-up.)
            preview.append_child(
                Element::builder("video", NS_WADDLE_LINK_PREVIEW)
                    .attr(xml_ncname!("url").to_owned(), native.url.as_str())
                    .attr(
                        xml_ncname!("media-type").to_owned(),
                        native.media_type.as_str(),
                    )
                    .build(),
            );
        }
        if let Some(player) = &data.player {
            let mut player_elem = Element::builder("player", NS_WADDLE_LINK_PREVIEW)
                .attr(xml_ncname!("url").to_owned(), player.url.as_str());
            if let Some(width) = player.width {
                player_elem = player_elem.attr(xml_ncname!("width").to_owned(), width.to_string());
            }
            if let Some(height) = player.height {
                player_elem =
                    player_elem.attr(xml_ncname!("height").to_owned(), height.to_string());
            }
            preview.append_child(player_elem.build());
        }
        lookup = lookup.append(preview);
    }
    Iq::Result {
        from: reply.from,
        to: reply.to,
        id: reply.id,
        payload: Some(lookup.build()),
    }
}

fn append_text(parent: &mut Element, name: &str, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    parent.append_child(
        Element::builder(name, NS_WADDLE_LINK_PREVIEW)
            .append(value.trim())
            .build(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::link_preview_telemetry::recorded_events;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn iq_get_with_payload(payload: Element) -> Iq {
        Iq::Get {
            from: None,
            to: None,
            id: "lookup-1".into(),
            payload,
        }
    }

    fn sender() -> FullJid {
        "alice@example.com/desktop".parse().expect("jid")
    }

    fn secret() -> &'static [u8] {
        b"test-link-preview-secret"
    }

    /// Register a live connection for [`sender`] — with both the connection
    /// registry and the authoritative `UserActor` the deferred delivery path
    /// resolves (mirroring production registration) — and return the
    /// outbound receiver.
    async fn register_requester(
        state: &WebSocketState,
    ) -> tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register(sender(), tx.clone());
        state
            .deps
            .protocol
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: sender(),
                entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
            })
            .await
            .expect("register user resource");
        rx
    }

    /// Await the deferred lookup reply on the requester's outbound channel
    /// and return `(iq id, lookup payload element)`.
    async fn recv_deferred_lookup_reply(
        rx: &mut tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
    ) -> (String, Element) {
        let outbound = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("deferred lookup reply within timeout")
            .expect("outbound channel open");
        assert!(
            matches!(
                outbound.kind,
                waddle_xmpp::registry::DeliveryKind::DirectFrame
            ),
            "deferred IQ reply is a server-generated direct frame"
        );
        let Stanza::Iq(iq) = outbound.stanza else {
            panic!("expected deferred IQ reply, got {:?}", outbound.stanza);
        };
        let Iq::Result { id, payload, .. } = *iq else {
            panic!("expected IQ result");
        };
        let payload = payload.expect("lookup payload");
        assert!(
            payload.is("lookup", NS_WADDLE_LINK_PREVIEW),
            "reply payload is the lookup element"
        );
        (id, payload)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handles_https_lookup_with_scoped_token_text_and_cached_image_metadata() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        let image_bytes = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nlookup preview");
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                    <meta property="og:title" content="Example Article">
                    <meta property="og:description" content="Plain text summary">
                    <meta property="og:image" content="{}/preview.png">
                    <meta property="og:image:width" content="640">
                    <meta property="og:image:height" content="360">
                    <meta property="og:image:alt" content="Article screenshot">
                  </head></html>"#,
                    server.uri()
                ),
                "text/html",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(image_bytes),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: std::sync::Arc<dyn crate::storage::BlobStorage> =
            std::sync::Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let state = create_test_websocket_state().await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
                state.deps.app_state.db_pool.global_actor().clone(),
                "alice@example.com".parse().expect("jid"),
            )),
            ..Default::default()
        };
        let lookup_url = format!("{}/a", server.uri());
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);
        let mut rx = register_requester(state.as_ref()).await;

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(resolver_policy),
            },
        )
        .await;

        assert!(
            response.is_empty(),
            "accepted lookup answers off the dispatch path: {response:?}"
        );
        let (id, lookup) = recv_deferred_lookup_reply(&mut rx).await;
        assert_eq!(id, "lookup-1");
        assert_eq!(lookup.attr("status"), Some("ready"));
        let preview = lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .expect("preview");
        assert_eq!(preview.attr("original-url"), Some(lookup_url.as_str()));
        assert_eq!(preview.attr("normalized-url"), Some(lookup_url.as_str()));
        assert!(preview.attr("token").is_some_and(|token| !token.is_empty()));
        assert!(preview.attr("expires-at").is_some());
        let image = preview
            .get_child("image", NS_WADDLE_LINK_PREVIEW)
            .expect("image");
        assert_eq!(image.attr("media-type"), Some("image/png"));
        assert_eq!(image.attr("width"), Some("640"));
        assert_eq!(image.attr("height"), Some("360"));
        assert_eq!(image.attr("alt"), Some("Article screenshot"));
        assert!(image.attr("url").is_some_and(|url| {
            url.starts_with("https://waddle.example/api/files/")
                && url.ends_with(".png")
                && url.contains("/link-preview-")
        }));
        let token = waddle_xmpp::xep::LinkPreviewToken::new(
            preview.attr("token").expect("token").to_string(),
        )
        .expect("token");
        let decoded =
            waddle_xmpp::xep::decode_link_preview_token(&token, secret(), i64::MIN).expect("token");
        assert_eq!(decoded.sender_jid.to_string(), "alice@example.com");
        assert_eq!(decoded.scope_jid.to_string(), "alice@example.com");
        let decoded_image = decoded.image.expect("token image");
        assert_eq!(decoded_image.media_type, PreviewImageMediaType::Png);
        assert_eq!(decoded_image.width, Some(640));
        assert_eq!(decoded_image.height, Some(360));
        assert_eq!(decoded_image.alt.as_deref(), Some("Article screenshot"));
        assert!(decoded_image
            .url
            .as_str()
            .starts_with("https://waddle.example/api/files/"));
        assert_eq!(
            preview
                .get_child("title", NS_WADDLE_LINK_PREVIEW)
                .map(Element::text)
                .as_deref(),
            Some("Example Article")
        );
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverReady),
            "ready lookup path must emit ready telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_video_lookup_mints_token_and_advertises_video() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8; 4096], "video/mp4"))
            .mount(&server)
            .await;
        let state = create_test_websocket_state().await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let lookup_url = format!("{}/clip.mp4", server.uri());
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);
        let mut rx = register_requester(state.as_ref()).await;

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(resolver_policy),
            },
        )
        .await;

        assert!(
            response.is_empty(),
            "accepted lookup answers off the dispatch path: {response:?}"
        );
        let (_, lookup) = recv_deferred_lookup_reply(&mut rx).await;
        assert_eq!(lookup.attr("status"), Some("ready"));
        let preview = lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .expect("preview");
        let video = preview
            .get_child("video", NS_WADDLE_LINK_PREVIEW)
            .expect("video element");
        assert_eq!(video.attr("media-type"), Some("video/mp4"));
        assert_eq!(video.attr("url"), Some(lookup_url.as_str()));
        assert_eq!(video.attr("size"), Some("4096"));
        assert!(
            preview.get_child("image", NS_WADDLE_LINK_PREVIEW).is_none(),
            "direct video preview has no cached image"
        );

        let token = waddle_xmpp::xep::LinkPreviewToken::new(
            preview.attr("token").expect("token").to_string(),
        )
        .expect("token");
        let decoded =
            waddle_xmpp::xep::decode_link_preview_token(&token, secret(), i64::MIN).expect("token");
        let decoded_video = decoded.video.expect("token video");
        assert_eq!(
            decoded_video.media_type,
            waddle_xmpp_core::DirectVideoMediaType::Mp4
        );
        assert_eq!(decoded_video.url.as_str(), lookup_url.as_str());
        assert_eq!(decoded_video.size, Some(4096));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_encoded_token_returns_failed_without_preview() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        let huge_path = format!("/{}", "a".repeat(8 * 1024));
        Mock::given(method("GET"))
            .and(path(huge_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"<html><head>
                    <meta property="og:title" content="Example Article">
                  </head></html>"#,
                "text/html",
            ))
            .mount(&server)
            .await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let lookup_url = format!("{}{}", server.uri(), huge_path);
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let mut rx = register_requester(state.as_ref()).await;
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(resolver_policy),
            },
        )
        .await;

        assert!(
            response.is_empty(),
            "accepted lookup answers off the dispatch path: {response:?}"
        );
        let (_, lookup) = recv_deferred_lookup_reply(&mut rx).await;
        assert_eq!(lookup.attr("status"), Some("failed"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverFailed),
            "token mint failure path must emit failed telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_https_lookup_returns_unsupported_without_token() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("http://example.com/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq(
            &iq,
            Some(&sender()),
            state.as_ref(),
            "muc.example.com",
            None,
            None,
            secret(),
        )
        .await;

        // Non-HTTPS is a deterministic pre-I/O verdict — answered inline on
        // the dispatch path, no resolve permit consumed.
        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("unsupported"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverUnsupported),
            "unsupported lookup path must emit unsupported telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disabled_policy_returns_blocked_without_token() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("https://example.com/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(LinkPreviewResolverPolicy {
                    enabled: false,
                    ..Default::default()
                }),
            },
        )
        .await;

        // A disabled resolver is a synchronous short-circuit — no fetch is
        // ever involved, so the reply stays on the dispatch path.
        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("blocked"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverBlocked),
            "disabled lookup path must emit blocked telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_host_policy_returns_blocked_lookup_without_token() {
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("https://blocked.example/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(LinkPreviewResolverPolicy {
                    blocked_hosts: vec!["blocked.example".parse().expect("pattern")],
                    ..Default::default()
                }),
            },
        )
        .await;

        // An operator-blocked host is a deterministic pre-I/O verdict —
        // answered inline on the dispatch path, no resolve permit consumed.
        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("blocked"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_returns_before_slow_resolution_and_reply_still_arrives() {
        // #1470 acceptance: a cold-cache resolve must not block dispatch —
        // the handler returns immediately while the resolver round-trip is
        // still in flight, and the requester still gets the result, matched
        // by the original IQ id, once resolution completes.
        let server = MockServer::start().await;
        let page_delay = std::time::Duration::from_secs(3);
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(page_delay)
                    .set_body_raw(
                        r#"<html><head>
                        <meta property="og:title" content="Slow Article">
                      </head></html>"#,
                        "text/html",
                    ),
            )
            .mount(&server)
            .await;
        let state = create_test_websocket_state().await;
        let mut rx = register_requester(state.as_ref()).await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            timeout: std::time::Duration::from_secs(10),
            ..Default::default()
        };
        let lookup_url = format!("{}/slow", server.uri());
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let dispatch_started = std::time::Instant::now();
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(resolver_policy),
            },
        )
        .await;
        let dispatch_elapsed = dispatch_started.elapsed();

        assert!(response.is_empty(), "no synchronous frames: {response:?}");
        assert!(
            dispatch_elapsed < page_delay,
            "dispatch must not wait for the resolver round-trip \
             (took {dispatch_elapsed:?} against a {page_delay:?} page delay)"
        );

        // A subsequent IQ through the same dispatch seam is fully answered
        // while the first lookup's resolver round-trip is still in flight —
        // the "stanzas queued behind a cold-cache preview" half of the
        // acceptance criteria. (The frame loop awaits exactly this handler,
        // so completing here is completing for the connection.)
        let second_iq = iq_get_with_payload(
            Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
                .append(
                    Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                        .append("https://example.com/next")
                        .build(),
                )
                .append(
                    Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                        .append("alice@example.com")
                        .build(),
                )
                .build(),
        );
        let second_response = handle_link_preview_lookup_iq_with_policy(
            &second_iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(LinkPreviewResolverPolicy {
                    enabled: false,
                    ..Default::default()
                }),
            },
        )
        .await;
        let second_elem: Element = second_response[0].parse().expect("second iq result");
        assert_eq!(
            second_elem
                .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
                .and_then(|lookup| lookup.attr("status")),
            Some("blocked"),
            "second stanza is fully dispatched and answered while the first \
             preview is still resolving"
        );

        let (id, lookup) = recv_deferred_lookup_reply(&mut rx).await;
        assert_eq!(id, "lookup-1", "deferred reply echoes the request id");
        assert_eq!(lookup.attr("status"), Some("ready"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_resolver_answers_failed_synchronously() {
        // Bounded admission (#1470): with every resolver permit taken, an
        // otherwise-valid lookup is answered `failed` inline (fail open,
        // #822) instead of queueing a deferred task.
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let state = create_test_websocket_state().await;
        let available = state
            .deps
            .protocol
            .link_preview_resolves
            .available_permits();
        let _hog = state
            .deps
            .protocol
            .link_preview_resolves
            .clone()
            .try_acquire_many_owned(u32::try_from(available).expect("permit count fits u32"))
            .expect("drain all resolver permits");
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("https://example.com/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(LinkPreviewResolverPolicy::default()),
            },
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("failed"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverSaturated),
            "saturated admission must emit saturated telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_host_verdict_survives_saturation_without_consuming_permit() {
        // Deterministic pre-I/O verdicts run before admission: with every
        // resolver permit drained, an operator-blocked host still answers
        // `blocked` (not `failed`) inline.
        let state = create_test_websocket_state().await;
        let available = state
            .deps
            .protocol
            .link_preview_resolves
            .available_permits();
        let _hog = state
            .deps
            .protocol
            .link_preview_resolves
            .clone()
            .try_acquire_many_owned(u32::try_from(available).expect("permit count fits u32"))
            .expect("drain all resolver permits");
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("https://blocked.example/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(LinkPreviewResolverPolicy {
                    blocked_hosts: vec!["blocked.example".parse().expect("pattern")],
                    ..Default::default()
                }),
            },
        )
        .await;

        assert_eq!(response.len(), 1, "exactly one inline reply");
        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(
            lookup.attr("status"),
            Some("blocked"),
            "policy verdict must not degrade to `failed` under saturation"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_reply_records_into_detached_sm_session_for_resume_replay() {
        // XEP-0198 parity with the old synchronous path: a reply whose
        // requester detached mid-resolve is recorded against the detached SM
        // session, so a later resume replays it instead of losing it.
        use waddle_xmpp::stream_management::SmSessionRegistry as _;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"<html><head><meta property="og:title" content="T"></head></html>"#,
                "text/html",
            ))
            .mount(&server)
            .await;
        let state = create_test_websocket_state().await;
        let requester = sender();
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(waddle_xmpp::stream_management::DetachedSession {
                stream_id: "preview-stream-1".to_string(),
                user_id: requester.to_string(),
                jid: requester.clone(),
                inbound_count: 1,
                outbound_count: 4,
                last_acked: 4,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: true,
                blocklist_interested: false,
                presence_available: true,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .await
            .expect("store detached session");
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(format!("{}/a", server.uri()).as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        // No live registry entry for the requester — only the detached
        // session above.
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&requester),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(resolver_policy),
            },
        )
        .await;
        assert!(response.is_empty());

        // The deferred reply lands in the detached session's unacked queue.
        let claimed = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let session = state
                    .deps
                    .protocol
                    .sm_session_registry
                    .claim_session("preview-stream-1")
                    .await
                    .expect("claim detached session")
                    .expect("session still stored");
                if !session.unacked_stanzas.is_empty() {
                    break session;
                }
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .release_claim("preview-stream-1")
                    .await
                    .expect("release claim between polls");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("deferred reply recorded for resume replay");
        assert_eq!(claimed.unacked_stanzas.len(), 1);
        assert!(
            claimed.unacked_stanzas[0].stanza_xml.contains("lookup-1")
                && claimed.unacked_stanzas[0]
                    .stanza_xml
                    .contains(NS_WADDLE_LINK_PREVIEW),
            "recorded stanza is the lookup reply: {}",
            claimed.unacked_stanzas[0].stanza_xml
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_reply_for_disconnected_requester_resolves_without_delivery() {
        // No registered connection and no detached SM session: the resolve
        // must still complete cleanly (previews fail open, #822) and the
        // reply is dropped without panicking the spawned task.
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"<html><head><meta property="og:title" content="T"></head></html>"#,
                "text/html",
            ))
            .mount(&server)
            .await;
        let state = create_test_websocket_state().await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(format!("{}/a", server.uri()).as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(resolver_policy),
            },
        )
        .await;
        assert!(response.is_empty());

        // The resolve completed (ready telemetry) even though delivery had
        // nowhere to go.
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverReady) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("resolve completes despite disconnected requester");
    }

    #[test]
    fn lookup_preview_includes_player_element() {
        let player_url =
            Url::parse("https://www.youtube-nocookie.com/embed/429A_VugWW0").expect("player url");
        let data = LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "alice@example.com".parse().expect("jid"),
            original_url: Url::parse("https://example.com/video").expect("url"),
            normalized_url: Url::parse("https://example.com/video").expect("url"),
            title: None,
            description: None,
            image: None,
            video: None,
            player: Some(LinkPreviewTokenPlayer {
                url: player_url.clone(),
                width: Some(1280),
                height: Some(720),
            }),
            native_video: None,
            expires_at_unix: 0,
        };
        let token =
            waddle_xmpp::xep::encode_link_preview_token_checked(&data, secret()).expect("token");
        let iq = iq_get_with_payload(Element::builder("lookup", NS_WADDLE_LINK_PREVIEW).build());
        let xml = build_link_preview_lookup_result(
            &iq,
            None,
            None,
            LinkPreviewResolverStatus::Ready,
            Some((data, token.as_str(), "2099-01-01T00:00:00.000Z".into())),
        );
        let elem: Element = xml.parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        let preview = lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .expect("preview");
        let player = preview
            .get_child("player", NS_WADDLE_LINK_PREVIEW)
            .expect("player element");
        assert_eq!(player.attr("url"), Some(player_url.as_str()));
        assert_eq!(player.attr("width"), Some("1280"));
        assert_eq!(player.attr("height"), Some("720"));
    }

    #[test]
    fn lookup_preview_includes_native_video_element() {
        let media_url =
            Url::parse("https://content.rawkode.academy/v/clip.mp4").expect("media url");
        let data = LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "alice@example.com".parse().expect("jid"),
            original_url: Url::parse("https://rawkode.academy/watch/yoke").expect("url"),
            normalized_url: Url::parse("https://rawkode.academy/watch/yoke").expect("url"),
            title: Some("Hands-on Yoke".to_string()),
            description: None,
            image: None,
            video: None,
            native_video: Some(LinkPreviewTokenNativeVideo {
                url: media_url.clone(),
                media_type: waddle_xmpp_core::DirectVideoMediaType::Mp4,
            }),
            player: None,
            expires_at_unix: 0,
        };
        let token =
            waddle_xmpp::xep::encode_link_preview_token_checked(&data, secret()).expect("token");
        let iq = iq_get_with_payload(Element::builder("lookup", NS_WADDLE_LINK_PREVIEW).build());
        let xml = build_link_preview_lookup_result(
            &iq,
            None,
            None,
            LinkPreviewResolverStatus::Ready,
            Some((data, token.as_str(), "2099-01-01T00:00:00.000Z".into())),
        );
        let elem: Element = xml.parse().expect("iq result");
        let preview = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .and_then(|lookup| lookup.get_child("preview", NS_WADDLE_LINK_PREVIEW))
            .expect("preview");
        // Native og:video surfaces as the same <video> element a direct-video
        // file uses, keeping the lookup result shape uniform. (The composer
        // preview currently renders only the image/text card; a composer video
        // card is a follow-up.)
        let video = preview
            .get_child("video", NS_WADDLE_LINK_PREVIEW)
            .expect("video element");
        assert_eq!(video.attr("url"), Some(media_url.as_str()));
        assert_eq!(video.attr("media-type"), Some("video/mp4"));
    }
}
