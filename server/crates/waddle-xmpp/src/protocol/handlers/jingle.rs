//! XEP-0166 Jingle session signaling.
//!
//! Validates that every `<content/>` carries a Waddle LiveKit
//! transport (`urn:waddle:transports:livekit:0`); rewrites the empty
//! `<transport/>` placeholder with a server-issued LiveKit join token
//! before forwarding the stanza to the peer; ACKs the requester.
//!
//! Sync handler — JWT signing is CPU-only and well under a
//! millisecond per stanza, so we hold an [`Arc<dyn SfuService>`]
//! directly rather than going through the two-phase AskSfu callback
//! machinery.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::{Action, Content, Jingle, SessionId, Transport};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{
    CallCorrelationId, CallId, Identity, MediaCapabilities, SessionBinding, SfuError, SfuService,
};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::handlers::session_initiate_rate_limit::{
    MujiActionRateLimit, SessionInitiateRateLimit, TerminateRateLimit,
};
use crate::protocol::traits::IqHandler;
use crate::telemetry::attributes::{
    CallControlRateLimitedSurface, CallSetupFailureReason, MetricAttribute, SfuDenialReason,
};
use crate::xep::xep0166::NS_JINGLE;
use crate::xep::xep0272::{find_muji, Muji};
use crate::xep::xep_waddle_livekit_transport::{
    IssuedTransport, TransportParseError, WaddleLiveKitTransport, NS_WADDLE_LIVEKIT_TRANSPORT,
};
use crate::Stanza;

mod undeliverable;

pub use undeliverable::{
    credential_free_jingle_echo, undeliverable_negotiation_rollback,
    UndeliverableNegotiationRollback,
};

/// XEP-0298 conference-info namespace. We use only the `isfocus`
/// attribute on a single element to signal that the Jingle peer
/// (the SFU mixer) is a conference focus rather than a P2P peer —
/// matching av-conferences ProtoXEP usage and the XEP-0298 §3.1
/// "Indicating a Focus" shape. The full COIN spec (participant
/// rosters, media descriptions per participant, etc.) is NOT
/// implemented; only this single discriminator is on the wire.
pub const NS_COIN: &str = "urn:xmpp:coin:1";
const PENDING_DM_INVITE_TTL: Duration = Duration::from_secs(10 * 60);

/// Mixer-JID prefix. A Muji-bearing Jingle session-initiate MUST
/// be addressed to `calls.<server-domain>` so the server can
/// distinguish it from a peer-to-peer Jingle on its own dispatcher.
/// Single seam to swap for an externalised XEP-0114 component
/// later if scaling demands.
pub const MIXER_LOCALPART: &str = "calls";

/// One in-flight Jingle call-setup attempt, for the
/// `waddle.call.setup.*` success-rate family (#1452).
///
/// Live only for `session-initiate`: `session-accept`,
/// `transport-info` and friends share the same code paths but are not
/// setups, and counting them would poison the denominator. When live,
/// construction has already counted `attempted`, and the caller must
/// finish it with exactly one [`CallSetupAttempt::ok`] or
/// [`CallSetupAttempt::failed`] on every return path.
#[derive(Clone, Copy)]
struct CallSetupAttempt {
    live: bool,
}

impl CallSetupAttempt {
    /// Open an attempt, counting `attempted` when this really is a
    /// `session-initiate`.
    fn open(is_session_initiate: bool) -> Self {
        if is_session_initiate {
            crate::telemetry::call::increment_call_setup_attempted();
        }
        Self {
            live: is_session_initiate,
        }
    }

    fn ok(self) {
        if self.live {
            crate::telemetry::call::increment_call_setup_ok();
        }
    }

    /// Hand a live attempt off to the routing layer instead of closing
    /// it here (#1488): the route disposition — delivered vs. no usable
    /// resource — is only known after the sans-I/O boundary, so the
    /// interpreter closes the attempt via the returned ticket.
    fn handed_to_router(self) -> Option<crate::telemetry::call::PendingCallSetupRoute> {
        self.live
            .then(crate::telemetry::call::PendingCallSetupRoute::open)
    }

    fn failed(self, reason: CallSetupFailureReason) {
        if self.live {
            crate::telemetry::call::increment_call_setup_failed(reason);
        }
    }
}

/// Build the mixer JID for a given server domain, e.g.
/// `calls.waddle.social` for domain `waddle.social`.
pub fn calls_mixer_jid(server_domain: &str) -> BareJid {
    let raw = format!("{MIXER_LOCALPART}.{server_domain}");
    raw.parse()
        .unwrap_or_else(|_| panic!("server domain produced an invalid mixer JID: {raw}"))
}

/// Predicate for the Jingle federation guard: returns `true` iff
/// `peer` is reachable without crossing a server boundary.
///
/// Two shapes count as local:
/// - the apex domain (regular P2P Jingle to another local account), or
/// - the local SFU mixer component JID (`calls.<ctx.domain>`, the
///   XEP-0272 Muji path).
fn is_local_jingle_peer(peer: &Jid, ctx: &StanzaContext<'_>) -> bool {
    if peer.domain() == ctx.full_jid.domain() {
        return true;
    }
    peer.to_bare() == calls_mixer_jid(ctx.domain)
}

/// Predicate for the Muji payload gate: returns `true` iff `room` is
/// a MUC room on this server's MUC service (`muc.<ctx.domain>`).
///
/// XEP-0272 §Joining lets the room JID range over any conference,
/// but accepting foreign rooms here would mint local LiveKit tokens
/// scoped to attacker-supplied call ids and pollute the participant
/// registry. Other servers should run their own SFU; this gate stops
/// us proxying as theirs.
fn is_local_muji_room(room: &BareJid, ctx: &StanzaContext<'_>) -> bool {
    room_is_on_local_muc_service(room, ctx.domain)
}

/// Whether `room` lives on this server's MUC service.
///
/// Single MUC service per server, derived from the apex like the rest
/// of the routing layer (`waddle-xmpp/src/routing.rs:80`). Public so
/// the websocket layer's #1445 relay pre-check uses the SAME predicate
/// as the handler's payload guard — two independent spellings of
/// "local MUC domain" would drift.
pub fn room_is_on_local_muc_service(room: &BareJid, server_domain: &str) -> bool {
    room.domain().as_str() == format!("muc.{server_domain}")
}

#[derive(Clone)]
pub struct JingleHandler {
    sfu: Arc<dyn SfuService>,
    pending_dm_invites: Arc<Mutex<HashMap<CallId, PendingDmInvite>>>,
    // Per-bare-JID rate limit on `session-initiate` only. Shared via
    // Arc so cloning the handler (the dispatcher clones it on
    // registration) keeps every clone hitting the same bucket map.
    session_initiate_rate_limit: Arc<SessionInitiateRateLimit>,
    terminate_rate_limit: Arc<TerminateRateLimit>,
    muji_action_rate_limit: Arc<MujiActionRateLimit>,
}

#[derive(Clone, Debug)]
struct PendingDmInvite {
    initiator: Identity,
    responder: Identity,
    created_at: Instant,
}

impl PendingDmInvite {
    fn new(initiator: Identity, responder: Identity) -> Self {
        Self {
            initiator,
            responder,
            created_at: Instant::now(),
        }
    }

    fn matches_parties(&self, initiator: &Identity, responder: &Identity) -> bool {
        &self.initiator == initiator && &self.responder == responder
    }

    fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.created_at) >= PENDING_DM_INVITE_TTL
    }
}

impl std::fmt::Debug for JingleHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JingleHandler").finish_non_exhaustive()
    }
}

impl JingleHandler {
    pub fn new(sfu: Arc<dyn SfuService>) -> Self {
        Self {
            sfu,
            pending_dm_invites: Arc::new(Mutex::new(HashMap::new())),
            session_initiate_rate_limit: Arc::new(SessionInitiateRateLimit::with_defaults()),
            terminate_rate_limit: Arc::new(TerminateRateLimit::with_defaults()),
            muji_action_rate_limit: Arc::new(MujiActionRateLimit::with_defaults()),
        }
    }

    /// Test-only constructor allowing a custom rate-limit policy.
    #[cfg(test)]
    pub fn with_rate_limit(
        sfu: Arc<dyn SfuService>,
        rate_limit: Arc<SessionInitiateRateLimit>,
    ) -> Self {
        Self {
            sfu,
            pending_dm_invites: Arc::new(Mutex::new(HashMap::new())),
            session_initiate_rate_limit: rate_limit,
            terminate_rate_limit: Arc::new(TerminateRateLimit::with_defaults()),
            muji_action_rate_limit: Arc::new(MujiActionRateLimit::with_defaults()),
        }
    }

    #[cfg(test)]
    pub fn with_rate_limits(
        sfu: Arc<dyn SfuService>,
        session_initiate_rate_limit: Arc<SessionInitiateRateLimit>,
        terminate_rate_limit: Arc<TerminateRateLimit>,
        muji_action_rate_limit: Arc<MujiActionRateLimit>,
    ) -> Self {
        Self {
            sfu,
            pending_dm_invites: Arc::new(Mutex::new(HashMap::new())),
            session_initiate_rate_limit,
            terminate_rate_limit,
            muji_action_rate_limit,
        }
    }
}

impl IqHandler for JingleHandler {
    fn namespace(&self) -> &'static str {
        NS_JINGLE
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let Some(jingle_elem) = iq_set_jingle(iq) else {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "expected IQ set with a <jingle/> payload",
            );
        };

        let jingle = match Jingle::try_from(jingle_elem.clone()) {
            Ok(j) => j,
            Err(_) => {
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "malformed <jingle/> stanza",
                );
            }
        };

        // #1452: from here on the action is known, so a rejection can
        // be attributed to the call-setup success rate. Pre-dispatch
        // rejections count the attempted/failed pair together because
        // the per-attempt tracker downstream never gets to open.
        let is_setup = matches!(jingle.action, Action::SessionInitiate);

        let Some(peer) = iq.to().cloned() else {
            if is_setup {
                crate::telemetry::call::record_call_setup_rejected(
                    CallSetupFailureReason::BadRequest,
                );
            }
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Jingle stanza missing 'to' attribute",
            );
        };

        // Same-domain guard. Federation isn't supported until the
        // cross-server token / SFU trust story is designed (see PR
        // description). Reject cross-domain Jingle stanzas at the
        // boundary with a clear `feature-not-implemented` so the
        // initiator sees an actionable error instead of a token
        // their peer's server can't make use of.
        //
        // "Local" means either the apex domain (P2P) or the local
        // SFU mixer's component JID (`calls.<ctx.domain>`, Muji path).
        // The mixer JID is a synthetic XMPP component identifier and
        // intentionally lives on its own subdomain to disambiguate it
        // from P2P Jingle on the dispatcher — it does NOT correspond
        // to a deployed host, so a strict `peer.domain() == ctx.full_jid.domain()`
        // would misclassify the local mixer as a remote server.
        if !is_local_jingle_peer(&peer, ctx) {
            if is_setup {
                crate::telemetry::call::record_call_setup_rejected(
                    CallSetupFailureReason::FederationUnsupported,
                );
            }
            return error_reply(
                iq,
                DefinedCondition::FeatureNotImplemented,
                "Jingle calling is currently single-domain only; federation is not yet supported",
            );
        }

        // XEP-0272 §Joining: a Jingle session-initiate may embed a
        // `<muji room='…'/>` element to signal "this Jingle is for
        // joining the SFU-mediated group call in that MUC room."
        // When present, we branch out of the P2P forwarding path and
        // act as the conference focus ourselves — minting a LiveKit
        // token, registering the participant, and replying with a
        // session-accept that carries the credentials + XEP-0298
        // `<conference-info isfocus='true'/>` marker.
        if let Some(muji_elem) = find_muji(jingle_elem) {
            let muji = match Muji::try_from(muji_elem) {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(error = %err, "rejecting Muji-bearing Jingle with malformed <muji/>");
                    if is_setup {
                        crate::telemetry::call::record_call_setup_rejected(
                            CallSetupFailureReason::BadRequest,
                        );
                    }
                    return error_reply(
                        iq,
                        DefinedCondition::BadRequest,
                        "malformed <muji/> element inside <jingle/>",
                    );
                }
            };
            return self.handle_muji_jingle(iq, jingle, muji, peer, ctx);
        }

        match jingle.action {
            Action::SessionInitiate => {
                // Rate-limit only session-initiate (creates a new SFU
                // registry entry + mints a JWT). Session-accept on an
                // existing initiate doesn't grow registry footprint
                // beyond what the matching initiate already paid for.
                let initiator_bare = ctx.full_jid.to_bare();
                if let Err(exceeded) = self
                    .session_initiate_rate_limit
                    .check_and_record(&initiator_bare)
                {
                    tracing::warn!(jid = %initiator_bare, %exceeded, "rate-limit dropped session-initiate");
                    crate::telemetry::call::record_call_setup_rejected(
                        CallSetupFailureReason::RateLimited,
                    );
                    return error_reply(
                        iq,
                        DefinedCondition::PolicyViolation,
                        "session-initiate rate limit exceeded",
                    );
                }
                self.handle_session_negotiation(iq, jingle, peer, ctx)
            }
            Action::SessionAccept => self.handle_session_negotiation(iq, jingle, peer, ctx),
            // The terminate limiter is charged INSIDE the handler, only
            // on the authorized mutating branches: charging before
            // validation would let one resource exhaust the shared
            // bare-JID bucket with syntactically valid unknown-session
            // terminates and starve a legitimate hangup from another
            // resource of the same account (#1612 review round 8).
            Action::SessionTerminate => self.handle_session_terminate(iq, jingle, peer, ctx),
            Action::SessionInfo
            | Action::TransportInfo
            | Action::ContentAdd
            | Action::ContentRemove
            | Action::ContentModify
            | Action::ContentAccept
            | Action::ContentReject
            | Action::TransportAccept
            | Action::TransportReject
            | Action::TransportReplace
            | Action::DescriptionInfo
            | Action::SecurityInfo => self.route_unchanged(iq, peer, ctx),
        }
    }
}

impl JingleHandler {
    /// Rewrite every Waddle LiveKit transport in the jingle stanza
    /// with a freshly-issued LiveKit join token for the responder,
    /// then forward to the responder. The server does NOT pre-ACK
    /// the requester — the responder's real client emits the
    /// `<iq type='result'/>` itself, per XEP-0166 §6.4.
    fn handle_session_negotiation(
        &self,
        iq: &Iq,
        mut jingle: Jingle,
        peer: Jid,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        let attempt = CallSetupAttempt::open(matches!(jingle.action, Action::SessionInitiate));
        let peer_full = match peer.clone().try_into_full() {
            Ok(full) => full,
            Err(_) => {
                attempt.failed(CallSetupFailureReason::BadRequest);
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "Jingle 'to' must be a full JID (resource required)",
                );
            }
        };
        let peer_identity = Identity::from_jid(peer_full);

        // Resolve the call initiator. `session-initiate` is scoped
        // to the authenticated sender. `session-accept` is addressed
        // back to the initiator, and XEP-0166 says non-initiate
        // actions SHOULD NOT carry `initiator`, so derive from `to`.
        let initiator_bare = match jingle.action {
            Action::SessionInitiate => match resolve_initiator(&jingle, ctx) {
                Ok(bare) => bare,
                Err(e) => {
                    attempt.failed(CallSetupFailureReason::NotAuthorized);
                    return e.into_reply(iq);
                }
            },
            Action::SessionAccept => peer.to_bare(),
            _ => ctx.full_jid.to_bare(),
        };
        if let Err(e) = validate_responder(&jingle, &jingle.action, ctx) {
            attempt.failed(CallSetupFailureReason::NotAuthorized);
            return e.into_reply(iq);
        }

        // Namespace the LiveKit room by the initiator's bare JID so
        // an attacker can't pick a sid that collides with a victim's
        // active call and have the server mint a join token scoped
        // to the victim's room.
        let call_id = match scoped_call_id(&initiator_bare, &jingle.sid.0) {
            Ok(c) => c,
            Err(_) => {
                attempt.failed(CallSetupFailureReason::BadRequest);
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "Jingle sid must be printable ASCII",
                );
            }
        };
        // #1452 correlation key: derived from the LiveKit room name
        // the client and the inbound webhook both already know, so all
        // three vantage points join on one bounded, non-PII value.
        let correlation = CallCorrelationId::for_call(&call_id);

        let self_identity = Identity::from_jid(ctx.full_jid.clone());
        let claimed_invite = if jingle.action == Action::SessionAccept {
            let mut pending = self.lock_pending_dm_invites();
            prune_expired_pending_dm_invites(&mut pending);
            match pending.remove(&call_id) {
                Some(invite) if invite.matches_parties(&peer_identity, &self_identity) => {
                    Some(invite)
                }
                Some(invite) => {
                    if !invite.is_expired(Instant::now()) {
                        pending.insert(call_id.clone(), invite);
                    }
                    attempt.failed(CallSetupFailureReason::NotAuthorized);
                    return error_reply(
                        iq,
                        DefinedCondition::Forbidden,
                        "Jingle responder was not invited to this call party",
                    );
                }
                None => {
                    attempt.failed(CallSetupFailureReason::NotAuthorized);
                    return error_reply(
                        iq,
                        DefinedCondition::Forbidden,
                        "Jingle responder was not invited to this call party",
                    );
                }
            }
        } else {
            None
        };

        // One join token per stanza, shared across contents (#1142).
        // 1:1 peers are symmetric, mutually-consenting participants:
        // full grants by construction, no role model applies.
        if let Err(reason) = rewrite_contents_transport(
            &mut jingle.contents,
            &call_id,
            &correlation,
            &peer_identity,
            MediaCapabilities::direct_call_peer(),
            &*self.sfu,
        ) {
            if let Some(invite) = claimed_invite.clone() {
                let mut pending = self.lock_pending_dm_invites();
                pending.entry(call_id.clone()).or_insert(invite);
            }
            attempt.failed(reason.setup_failure_reason());
            // 1:1 P2P path: the requester addressed the
            // session-initiate to `peer`, so the Jingle-level
            // rejection must appear from that peer resource,
            // not from the requester back to itself.
            return reason.into_error_reply(iq, &jingle.sid, &peer);
        }

        if claimed_invite.is_some() && self.lock_pending_dm_invites().contains_key(&call_id) {
            attempt.failed(CallSetupFailureReason::NotAuthorized);
            return error_reply(
                iq,
                DefinedCondition::Forbidden,
                "Jingle responder was superseded before this accept completed",
            );
        }

        // Register the authenticated sender and, for the initial
        // invite, the addressed responder whose token was just minted.
        // The pending full-JID invite is the later session-accept
        // authorization proof; the SFU registry is accounting and
        // token revocation state only.
        self.sfu.register_call_participant(&call_id, &self_identity);
        if jingle.action == Action::SessionInitiate {
            revoke_other_dm_participants(&*self.sfu, &call_id, &self_identity, &peer_identity);
            self.sfu.register_call_participant(&call_id, &peer_identity);
            let mut pending = self.lock_pending_dm_invites();
            prune_expired_pending_dm_invites(&mut pending);
            pending.insert(
                call_id.clone(),
                PendingDmInvite::new(self_identity.clone(), peer_identity.clone()),
            );
        }

        // Stamp the forwarded stanza's `from` with the authenticated
        // JID — never trust the client-supplied `iq.from`.
        let forwarded_elem: Element = jingle.into();
        let forwarded_iq = Iq::Set {
            from: Some(ctx.full_jid.clone().into()),
            to: Some(peer),
            id: iq.id().to_string(),
            payload: forwarded_elem,
        };

        vec![OutboundEvent::RouteToConnection {
            jid: forwarded_iq
                .to()
                .cloned()
                .unwrap_or_else(|| ctx.full_jid.clone().into()),
            stanza: Box::new(Stanza::Iq(Box::new(forwarded_iq))),
            call_setup: attempt.handed_to_router(),
        }]
    }

    /// Handle a Muji-bearing Jingle stanza (XEP-0272 §Joining + a
    /// custom SFU-focus interpretation). Routes by `jingle.action`:
    ///
    /// - `session-initiate`: mint a LiveKit token for the calling
    ///   identity, register them with the SFU under the room JID
    ///   (`<muji room='…'/>` is authoritative — NOT `iq.to`, which
    ///   could theoretically be a different mixer alias), and reply
    ///   with a `session-accept` carrying the credentials and a
    ///   `<conference-info xmlns='urn:xmpp:coin:1' isfocus='true'/>`
    ///   marker per XEP-0298.
    /// - `session-terminate`: unregister the participant. Reply
    ///   `<iq type='result'/>` per XEP-0166 §6.7.
    /// - Anything else: reject with `<bad-request/>` — Muji peer
    ///   exchanges (transport-info etc.) aren't meaningful when the
    ///   peer is a focus that brokers tokens.
    fn handle_muji_jingle(
        &self,
        iq: &Iq,
        jingle: Jingle,
        muji: Muji,
        peer: Jid,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        // The mixer JID is `calls.<server-domain>`. Reject Muji
        // sessions addressed elsewhere so a malicious client can't
        // route a Muji session-initiate through a different
        // server-side component and pick up tokens.
        let attempt = CallSetupAttempt::open(matches!(jingle.action, Action::SessionInitiate));
        let expected_mixer = calls_mixer_jid(ctx.domain);
        if peer.to_bare() != expected_mixer {
            attempt.failed(CallSetupFailureReason::BadRequest);
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Muji Jingle sessions must be addressed to the calls mixer JID",
            );
        }

        if jingle.action == Action::SessionInitiate {
            // Same rate limit as 1:1 session-initiate, and for the
            // same reason: a Muji initiate mints a JWT and grows the
            // SFU registry. It costs strictly more than the 1:1 case
            // since #1445 — an initiate for a room this replica does
            // not own also buys a claim-store lookup and a cross-node
            // relay — so the unlimited Muji branch would be the
            // cheapest amplification primitive in the call path.
            let initiator_bare = ctx.full_jid.to_bare();
            if let Err(exceeded) = self
                .session_initiate_rate_limit
                .check_and_record(&initiator_bare)
            {
                tracing::warn!(
                    jid = %initiator_bare,
                    %exceeded,
                    "rate-limit dropped Muji session-initiate"
                );
                attempt.failed(CallSetupFailureReason::RateLimited);
                return error_reply(
                    iq,
                    DefinedCondition::PolicyViolation,
                    "session-initiate rate limit exceeded",
                );
            }
            // XEP-0166 §7.1 spoofing defense: when present on
            // session-initiate, `initiator` must match the
            // authenticated session. Non-initiate actions should not
            // carry it, so we ignore it there per the XEP.
            if let Err(e) = resolve_muji_initiator(&jingle, ctx) {
                if let Some(room_jid) = muji.room.as_ref() {
                    record_sfu_token_authorization_denial(room_jid, &ctx.full_jid.to_bare());
                }
                attempt.failed(CallSetupFailureReason::NotAuthorized);
                return e.into_reply(iq);
            }
        }

        let Some(room_jid) = muji.room.clone() else {
            attempt.failed(CallSetupFailureReason::BadRequest);
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Muji <muji/> child inside <jingle/> requires the 'room' attribute",
            );
        };

        // The `<muji room='…'/>` JID must point at a MUC room on
        // THIS server's MUC service (`muc.<ctx.domain>`). Without
        // this check a local user could mint a LiveKit token whose
        // SFU room id is `victim@muc.other.example`, polluting our
        // LiveKit namespace with arbitrary attacker-supplied call
        // ids and (worse) registering themselves into a participant
        // registry keyed by a foreign room jid that local presence
        // pumps may join later. The federation guard above is a
        // peer-JID gate; this is a payload gate.
        if !is_local_muji_room(&room_jid, ctx) {
            attempt.failed(CallSetupFailureReason::BadRequest);
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Muji room JID must reference a MUC room on this server",
            );
        }

        match jingle.action {
            Action::SessionInitiate => {
                self.handle_muji_session_initiate(iq, jingle, room_jid, ctx, attempt)
            }
            // The terminate limiter is charged INSIDE the handler on the
            // authorized mutating branch only (#1612 review round 10):
            // charging before `has_call_participant` would let bogus
            // unknown-session terminates exhaust the shared bare-JID
            // bucket and starve a legitimate hangup from another
            // resource of the same account.
            Action::SessionTerminate => {
                self.handle_muji_session_terminate(iq, jingle, room_jid, ctx)
            }
            _ => {
                let initiator_bare = ctx.full_jid.to_bare();
                if let Err(exceeded) = self
                    .muji_action_rate_limit
                    .check_and_record(&initiator_bare)
                {
                    tracing::warn!(
                        jid = %initiator_bare,
                        %exceeded,
                        "rate-limit dropped Muji non-initiate action"
                    );
                    crate::telemetry::call::increment_call_control_rate_limited(
                        CallControlRateLimitedSurface::MujiAction,
                    );
                    return error_reply(
                        iq,
                        DefinedCondition::PolicyViolation,
                        "Muji action rate limit exceeded",
                    );
                }
                error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "Muji Jingle supports only session-initiate and session-terminate",
                )
            }
        }
    }

    /// Build the session-accept that replies to a Muji
    /// `session-initiate`. The CallId is the room JID itself (one
    /// SFU room per MUC room — every occupant who joins the call
    /// lands in the same LiveKit room), NOT `scoped_call_id`. That's
    /// the deliberate semantic difference from the 1:1 path.
    fn handle_muji_session_initiate(
        &self,
        iq: &Iq,
        mut jingle: Jingle,
        room_jid: BareJid,
        ctx: &StanzaContext<'_>,
        attempt: CallSetupAttempt,
    ) -> Vec<OutboundEvent> {
        let call_id = match CallId::new(room_jid.to_string()) {
            Ok(c) => c,
            Err(_) => {
                attempt.failed(CallSetupFailureReason::BadRequest);
                return error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "Muji room JID could not be normalised into a SFU call id",
                );
            }
        };
        let correlation = CallCorrelationId::for_call(&call_id);

        // Identity is the authenticated full JID. A Muji
        // session-initiate from alice@waddle.test/desktop mints a
        // token for THAT resource specifically, so alice/mobile
        // joining the same call gets her own token under her own
        // identity — multi-resource correct.
        let identity = Identity::from_jid(ctx.full_jid.clone());

        // Rewrite the contents' transports with ONE issued token
        // shared across every content (#1142): LiveKit's identity
        // model is "one identity per participant"; the audio/video
        // split lives below the LiveKit layer.
        // Mixer JID becomes the `from` of any session-terminate
        // we emit per XEP-0166 §10.2 — the conference focus is the
        // source of the rejection, not the requester.
        let mixer_jid: Jid = calls_mixer_jid(ctx.domain).into();
        // Grants come from the websocket layer's Muji gate, which
        // derived them from the sender's current XEP-0045 role at
        // authorization time. Absence means this Muji IQ reached the
        // mint through a dispatch route that never ran the gate, so no
        // authorization decision exists for it. Refuse outright rather
        // than minting a listen-only token: a subscribe-capable JWT
        // still admits the holder to the room's media and would let an
        // unverified caller listen in.
        let Some(capabilities) = ctx.media_capabilities else {
            record_sfu_token_authorization_denial(&room_jid, &ctx.full_jid.to_bare());
            attempt.failed(CallSetupFailureReason::NotAuthorized);
            return error_reply(
                iq,
                DefinedCondition::Forbidden,
                "Muji join was not authorized: no MUC membership decision accompanied this request",
            );
        };
        if let Err(reason) = rewrite_contents_transport(
            &mut jingle.contents,
            &call_id,
            &correlation,
            &identity,
            capabilities,
            &*self.sfu,
        ) {
            attempt.failed(reason.setup_failure_reason());
            return reason.into_error_reply(iq, &jingle.sid, &mixer_jid);
        }
        self.sfu.register_call_participant(&call_id, &identity);
        // Bind the registration to this session-initiate's Jingle sid
        // (#1608): a later terminate carrying a DIFFERENT sid is a
        // stale leftover from a previous call in the same room and
        // must not tear this session down. A rejoin re-registers and
        // rebinds, so the stored binding always names the newest
        // session. `xmpp_parsers` puts no length cap on a Jingle sid,
        // so a pathological over-cap sid fails to bind; the
        // registration then stays unbound and keeps the room-scoped
        // pre-#1608 teardown — the exposure is confined to that
        // client's own session.
        if let Ok(session) = SessionBinding::new(jingle.sid.0.clone()) {
            self.sfu
                .bind_participant_session(&call_id, &identity, &session);
        }

        // XEP-0166 §6.3 ack: respond to the session-initiate IQ
        // with an EMPTY IQ result IMMEDIATELY. The session-accept
        // is then delivered as a SEPARATE server-initiated
        // `<iq type='set'>` stanza per the same section. Bundling
        // accept and ack in one stanza is non-conformant to §6.3
        // and breaks interop with strict Muji peers.
        let ack = Iq::Result {
            from: iq.to().cloned(),
            to: iq.from().cloned(),
            id: iq.id().to_string(),
            payload: None,
        };

        // Build the session-accept `<jingle/>` Element explicitly
        // to control child ordering: XEP-0272 §Joining shows
        // `<muji>` as the FIRST child of `<jingle/>`, BEFORE any
        // `<content/>`. Going through `Jingle::into()` would
        // serialise contents first and force us to append the
        // `<muji/>` / `<conference-info/>` at the tail —
        // non-conformant with the XEP example. Constructing the
        // element directly here keeps the wire shape strictly
        // matching the spec.
        let responder: Jid = calls_mixer_jid(ctx.domain).into();
        let mut jingle_builder = Element::builder("jingle", NS_JINGLE)
            .attr(
                minidom::rxml::xml_ncname!("action").to_owned(),
                "session-accept",
            )
            .attr(
                minidom::rxml::xml_ncname!("sid").to_owned(),
                jingle.sid.0.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("responder").to_owned(),
                responder.to_string(),
            );
        // 1. `<muji room='…'/>` first per XEP-0272 §Joining example.
        jingle_builder = jingle_builder.append(Muji::for_room(room_jid).to_element());
        // 2. XEP-0298 §3.1 focus marker — also a direct child of
        //    `<jingle/>`. Stamped on the accept so the client can
        //    distinguish this from a P2P session-accept (a normal
        //    peer would never set `isfocus='true'`).
        jingle_builder = jingle_builder.append(
            Element::builder("conference-info", NS_COIN)
                .attr(minidom::rxml::xml_ncname!("isfocus").to_owned(), "true")
                .build(),
        );
        // 3. Then the rewritten `<content/>` children carrying the
        //    issued LiveKit transport.
        for content in jingle.contents {
            jingle_builder = jingle_builder.append(Element::from(content));
        }
        let accept_elem = jingle_builder.build();

        // Server-initiated session-accept as a SEPARATE IQ-set per
        // XEP-0166 §6.3. The client ACKs it with an empty IQ
        // result of its own; that ack is silently discarded by
        // the server's IQ dispatcher (no state machine needed —
        // the SFU registration already happened atomically with
        // the session-initiate above).
        let session_accept_id = format!("muji-accept-{}", uuid::Uuid::new_v4());
        let session_accept = Iq::Set {
            from: Some(responder),
            to: Some(ctx.full_jid.clone().into()),
            id: session_accept_id,
            payload: accept_elem,
        };

        attempt.ok();
        vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(ack)))),
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(session_accept)))),
        ]
    }

    fn handle_muji_session_terminate(
        &self,
        iq: &Iq,
        jingle: Jingle,
        room_jid: BareJid,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        // Same CallId derivation as session-initiate so the
        // unregister matches the original registration.
        if let Ok(call_id) = CallId::new(room_jid.to_string()) {
            let sender_identity = Identity::from_jid(ctx.full_jid.clone());
            if !self.sfu.has_call_participant(&call_id, &sender_identity) {
                return unknown_session_reply(iq);
            }
            // #1608: the registration is room-scoped, but teardown
            // must be session-scoped. A terminate whose sid differs
            // from the binding recorded at session-initiate is a
            // stale leftover of a PREVIOUS call in this room (e.g. a
            // deferred client flush landing after a rejoin) and gets
            // unknown-session instead of ending the live session. An
            // unbound registration (webhook-restored, or a stub that
            // does not track bindings) accepts any sid, as before.
            if let Some(bound) = self
                .sfu
                .participant_session_binding(&call_id, &sender_identity)
            {
                if bound.as_str() != jingle.sid.0.as_str() {
                    tracing::warn!(
                        room = %room_jid,
                        sender = %ctx.full_jid,
                        stale_sid = %jingle.sid.0,
                        "refusing stale-sid Muji session-terminate for a live session"
                    );
                    return unknown_session_reply(iq);
                }
            }
            // Charge only the authorized mutating teardown (#1612
            // review round 10): unknown-session rejections above stay
            // uncharged, so bogus terminates cannot exhaust the shared
            // bare-JID budget. The websocket layer's own bucket only
            // covers the cross-node relay path; locally-owned rooms
            // are bounded here.
            if let Some(reply) = self.check_terminate_rate_limit(iq, ctx) {
                return reply;
            }
            let _ = self
                .sfu
                .unregister_call_participant(&call_id, &sender_identity, None);
        }
        // Empty IQ result per XEP-0166 §6.7.
        let mixer: Jid = calls_mixer_jid(ctx.domain).into();
        let reply = Iq::Result {
            from: Some(mixer),
            to: Some(ctx.full_jid.clone().into()),
            id: iq.id().to_string(),
            payload: None,
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
            reply,
        ))))]
    }

    /// Handle a 1:1 `session-terminate` (#1131).
    ///
    /// Authorization is on the SENDER's call membership — requiring
    /// the addressed peer to still be registered meant a survivor
    /// whose peer had already been swept (crashed client cleaned up
    /// via the LiveKit webhook or the reconciler) got `<forbidden/>`
    /// and NO cleanup: their registration and un-revoked JTIs
    /// lingered until reconciliation.
    ///
    /// Resolution order over the two sid-scoped call-id candidates
    /// (`<sender-bare>::<sid>`, `<peer-bare>::<sid>`):
    ///
    /// 1. Both sender and peer registered → unregister both (revoking
    ///    their JTIs) and forward the terminate to the peer, whose
    ///    client acks per XEP-0166 §6.7.
    /// 2. Sender is the ONLY remaining registered party (survivor
    ///    terminate) → unregister the sender and ack the IQ on the
    ///    departed peer's behalf — the peer is gone, so there is
    ///    nobody left to forward to.
    /// 3. A candidate call still has participants but the sender+peer
    ///    pairing is not among them → `<forbidden/>`: a third party
    ///    (or a revoked/superseded responder) must not tear down or
    ///    probe someone else's call.
    /// 4. No candidate call exists at all (duplicate terminate,
    ///    terminate glare, long-dead session) → `<item-not-found/>` +
    ///    `<unknown-session/>` per the XEP-0166 error table — never
    ///    `<forbidden/>`, and idempotent (no state is touched).
    fn handle_session_terminate(
        &self,
        iq: &Iq,
        jingle: Jingle,
        peer: Jid,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        let Ok(peer_full) = peer.clone().try_into_full() else {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Jingle 'to' must be a full JID (resource required)",
            );
        };
        let ctx_bare = ctx.full_jid.to_bare();
        let peer_bare = peer.to_bare();
        let sender_identity = Identity::from_jid(ctx.full_jid.clone());
        let peer_identity = Identity::from_jid(peer_full);

        let candidates: Vec<CallId> = [ctx_bare, peer_bare]
            .into_iter()
            .filter_map(|initiator_bare| scoped_call_id(&initiator_bare, &jingle.sid.0).ok())
            .collect();

        // Case 1: the fully-live session — both parties registered.
        if let Some(call_id) = candidates.iter().find(|call_id| {
            self.sfu.has_call_participant(call_id, &sender_identity)
                && self.sfu.has_call_participant(call_id, &peer_identity)
        }) {
            // Charge the limiter only now that this is a validated,
            // mutating teardown of the sender's own session.
            if let Some(reply) = self.check_terminate_rate_limit(iq, ctx) {
                return reply;
            }
            let _ = self
                .sfu
                .unregister_call_participant(call_id, &sender_identity, None);
            let _ = self
                .sfu
                .unregister_call_participant(call_id, &peer_identity, None);
            self.lock_pending_dm_invites().remove(call_id);
            return self.route_unchanged(iq, peer, ctx);
        }

        // Case 2: survivor terminate — the peer's registration is
        // gone (webhook / reconciler sweep) and the sender is the
        // only remaining party. Unregister the sender (revoking its
        // JTIs) and ack per XEP-0166 §6.7 on the departed peer's
        // behalf.
        if let Some(call_id) = candidates.iter().find(|call_id| {
            let participants = self.sfu.participants_for_call(call_id);
            !participants.is_empty() && participants.iter().all(|p| p == &sender_identity)
        }) {
            // Same charge point as case 1: an authorized survivor
            // teardown that will mutate registry state.
            if let Some(reply) = self.check_terminate_rate_limit(iq, ctx) {
                return reply;
            }
            let _ = self
                .sfu
                .unregister_call_participant(call_id, &sender_identity, None);
            self.lock_pending_dm_invites().remove(call_id);
            return terminate_ack(iq);
        }

        // Case 3: some candidate call is live but the sender+peer
        // pairing is not part of it — a third party or a superseded
        // responder probing/terminating someone else's call.
        if candidates
            .iter()
            .any(|call_id| !self.sfu.participants_for_call(call_id).is_empty())
        {
            return error_reply(
                iq,
                DefinedCondition::Forbidden,
                "Jingle terminator is not a participant in this call",
            );
        }

        // Case 4: fully-unknown session — XEP-0166 unknown-session.
        unknown_session_reply(iq)
    }

    /// Forward the stanza verbatim with a sanitised `from` so the
    /// authenticated session is the only thing the peer sees as the
    /// sender. Used for transport-info, session-info, content-* and
    /// the like that don't need server mediation.
    fn route_unchanged(&self, iq: &Iq, peer: Jid, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let (header, payload) = iq.clone().split();
        let forwarded = xmpp_parsers::iq::Iq::assemble(
            xmpp_parsers::iq::IqHeader {
                from: Some(ctx.full_jid.clone().into()),
                to: Some(peer.clone()),
                id: header.id,
            },
            payload,
        );
        vec![OutboundEvent::RouteToConnection {
            jid: peer,
            stanza: Box::new(Stanza::Iq(Box::new(forwarded))),
            call_setup: None,
        }]
    }

    fn lock_pending_dm_invites(&self) -> MutexGuard<'_, HashMap<CallId, PendingDmInvite>> {
        self.pending_dm_invites.lock().unwrap_or_else(|err| {
            tracing::warn!("recovering poisoned pending DM invite lock");
            err.into_inner()
        })
    }

    fn check_terminate_rate_limit(
        &self,
        iq: &Iq,
        ctx: &StanzaContext<'_>,
    ) -> Option<Vec<OutboundEvent>> {
        let initiator_bare = ctx.full_jid.to_bare();
        match self.terminate_rate_limit.check_and_record(&initiator_bare) {
            Ok(_) => None,
            Err(exceeded) => {
                tracing::warn!(
                    jid = %initiator_bare,
                    %exceeded,
                    "rate-limit dropped session-terminate"
                );
                crate::telemetry::call::increment_call_control_rate_limited(
                    CallControlRateLimitedSurface::Terminate,
                );
                Some(error_reply(
                    iq,
                    DefinedCondition::PolicyViolation,
                    "session-terminate rate limit exceeded",
                ))
            }
        }
    }
}

#[derive(Debug)]
enum InitiatorError {
    InitiatorMismatch,
    ResponderMismatch,
}

impl InitiatorError {
    fn into_reply(self, iq: &Iq) -> Vec<OutboundEvent> {
        match self {
            Self::InitiatorMismatch => error_reply(
                iq,
                DefinedCondition::Forbidden,
                "Jingle initiator must match the authenticated session",
            ),
            Self::ResponderMismatch => error_reply(
                iq,
                DefinedCondition::Forbidden,
                "Jingle responder must match the authenticated session",
            ),
        }
    }
}

/// Verify the Jingle stanza's `initiator` attribute is consistent
/// with the authenticated session and return the initiator's bare
/// JID used to derive the SFU call-id.
fn resolve_initiator(jingle: &Jingle, ctx: &StanzaContext<'_>) -> Result<BareJid, InitiatorError> {
    let ctx_bare = ctx.full_jid.to_bare();
    // The initiator attribute MAY be present (XEP-0166 §7.1); if so
    // it must name the authenticated full JID. A same-bare but
    // different-resource initiator would let alice/mobile mint or
    // forward a call as alice/desktop.
    let authenticated = Jid::from(ctx.full_jid.clone());
    match jingle.initiator.as_ref() {
        Some(declared) if declared != &authenticated => Err(InitiatorError::InitiatorMismatch),
        _ => Ok(ctx_bare),
    }
}

fn validate_responder(
    jingle: &Jingle,
    action: &Action,
    ctx: &StanzaContext<'_>,
) -> Result<(), InitiatorError> {
    if *action != Action::SessionAccept {
        return Ok(());
    }
    let authenticated = Jid::from(ctx.full_jid.clone());
    match jingle.responder.as_ref() {
        Some(declared) if declared == &authenticated => Ok(()),
        Some(_) => Err(InitiatorError::ResponderMismatch),
        None => Ok(()),
    }
}

/// Muji uses the same XEP-0166 initiator rule as 1:1 Jingle when
/// the attribute is present: it must name the authenticated full
/// JID. The attribute is still optional on session-initiate, so an
/// omitted value resolves to the authenticated session.
fn resolve_muji_initiator(
    jingle: &Jingle,
    ctx: &StanzaContext<'_>,
) -> Result<BareJid, InitiatorError> {
    let ctx_full = Jid::from(ctx.full_jid.clone());
    match jingle.initiator.as_ref() {
        Some(declared) if declared == &ctx_full => Ok(ctx.full_jid.to_bare()),
        Some(_) => Err(InitiatorError::InitiatorMismatch),
        None => Ok(ctx.full_jid.to_bare()),
    }
}

/// The 1:1 LiveKit room name: `{initiator_bare}::{sid}`.
///
/// The chat client re-derives this format (`dmCallRoomName` in
/// `chat/src/lib/calls/call-correlation.ts`) so lifecycle events for
/// calls that never connected still carry the shared correlation id —
/// changing this format requires changing the client twin, and the
/// cross-pinned digest tests on both sides fail until they agree.
fn scoped_call_id(initiator_bare: &BareJid, sid: &str) -> Result<CallId, SfuError> {
    CallId::new(format!("{}::{}", initiator_bare, sid))
}

enum RewriteError {
    UnsupportedTransport,
    InvalidWaddleTransport(TransportParseError),
    ClientSuppliedIssuedTransport,
    SfuFailed,
}

impl RewriteError {
    /// Bucket this rewrite failure into the closed call-setup failure
    /// taxonomy (#1452). Keeps the success-rate reasons independent of
    /// the wire-shape the error maps to.
    fn setup_failure_reason(&self) -> CallSetupFailureReason {
        match self {
            Self::UnsupportedTransport | Self::ClientSuppliedIssuedTransport => {
                CallSetupFailureReason::UnsupportedTransport
            }
            Self::InvalidWaddleTransport(_) => CallSetupFailureReason::BadRequest,
            Self::SfuFailed => CallSetupFailureReason::TokenMintFailed,
        }
    }

    /// Convert the rewrite error into the appropriate outbound
    /// stanza shape. `UnsupportedTransport` follows the XEP-0166
    /// §10.2 "Recovering from a Negotiation Failure" pattern:
    /// empty IQ-result ack + a SEPARATE server-initiated
    /// `<iq type='set'><jingle action='session-terminate'>
    /// <reason><unsupported-transports/></reason></jingle></iq>`
    /// — the Jingle-specific reason condition for an unacceptable
    /// transport method. The other variants stay as stanza errors
    /// because they represent protocol-level errors (XEP-0166
    /// §10.4 / RFC 6120 stanza errors) rather than Jingle
    /// negotiation failures with a defined `<reason/>` condition.
    fn into_error_reply(self, iq: &Iq, sid: &SessionId, from: &Jid) -> Vec<OutboundEvent> {
        match self {
            Self::UnsupportedTransport => unsupported_transports_termination(iq, sid, from),
            // Don't reflect the inner error string to the client —
            // it can leak parser internals or signing details. Log
            // it server-side and return a generic message.
            Self::InvalidWaddleTransport(inner) => {
                tracing::warn!(error = %inner, "rejecting Jingle stanza with invalid Waddle transport");
                error_reply(
                    iq,
                    DefinedCondition::BadRequest,
                    "invalid Waddle LiveKit transport",
                )
            }
            Self::ClientSuppliedIssuedTransport => error_reply(
                iq,
                DefinedCondition::BadRequest,
                "Waddle LiveKit transport credentials must be server-issued",
            ),
            Self::SfuFailed => {
                error_reply(iq, DefinedCondition::InternalServerError, "internal error")
            }
        }
    }
}

/// XEP-0166 §10.2 conformant rejection of a session-initiate with
/// an unsupported transport. Emits two stanzas:
///
/// 1. Empty `<iq type='result'/>` ack of the session-initiate (the
///    XEP-0166 §6.3 IQ ack — required because the responder must
///    acknowledge the stanza before any Jingle-level negotiation
///    failure is communicated).
/// 2. Server-initiated `<iq type='set'>` carrying
///    `<jingle action='session-terminate'><reason>
///    <unsupported-transports/></reason></jingle>`. This is the
///    Jingle-specific reason condition for "the transport method
///    is unacceptable" per XEP-0166 §7.4.
///
/// `from` is the JID the server presents as the source of the
/// session-terminate. For 1:1 P2P sessions this is the
/// authenticated full JID (the peer the requester tried to call);
/// for Muji sessions this is the mixer JID.
fn unsupported_transports_termination(iq: &Iq, sid: &SessionId, from: &Jid) -> Vec<OutboundEvent> {
    let ack = Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_string(),
        payload: None,
    };
    let terminate = crate::xep::xep0166::session_terminate(
        sid.clone(),
        xmpp_parsers::jingle::Reason::UnsupportedTransports,
    );
    let terminate_iq = Iq::Set {
        from: Some(from.clone()),
        to: iq.from().cloned(),
        id: format!("jingle-terminate-{}", uuid::Uuid::new_v4()),
        payload: terminate.into(),
    };
    vec![
        OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(ack)))),
        OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(terminate_iq)))),
    ]
}

/// Validate that `content` carries the empty Waddle LiveKit transport
/// placeholder (`urn:waddle:transports:livekit:0` in its `Request`
/// form). Client-supplied issued credentials and foreign transports
/// are rejected without minting anything.
fn validate_transport_placeholder(content: &Content) -> Result<(), RewriteError> {
    let Some(Transport::Unknown(transport_elem)) = &content.transport else {
        return Err(RewriteError::UnsupportedTransport);
    };
    if transport_elem.ns() != NS_WADDLE_LIVEKIT_TRANSPORT {
        return Err(RewriteError::UnsupportedTransport);
    }
    let parsed = WaddleLiveKitTransport::try_from(transport_elem)
        .map_err(RewriteError::InvalidWaddleTransport)?;
    match parsed {
        WaddleLiveKitTransport::Issued(_) => Err(RewriteError::ClientSuppliedIssuedTransport),
        WaddleLiveKitTransport::Request => Ok(()),
    }
}

/// Rewrite every `<content/>`'s transport placeholder with ONE
/// freshly-issued LiveKit join token shared across all contents.
///
/// #1142: LiveKit's identity model is "one identity per participant";
/// the audio/video split lives below the LiveKit layer, so all
/// contents of a negotiation stanza must share the same credential.
/// Minting per content burned one JTI per `<content/>` — a few
/// two-content renegotiations pushed still-live JTIs out of the
/// 16-slot per-participant FIFO, leaving them unrevocable on hangup.
///
/// Every placeholder is validated *before* the single mint so a
/// malformed later content cannot leave an orphaned JTI in the
/// tracker.
fn rewrite_contents_transport(
    contents: &mut [Content],
    call_id: &CallId,
    correlation: &CallCorrelationId,
    peer_identity: &Identity,
    capabilities: MediaCapabilities,
    sfu: &dyn SfuService,
) -> Result<(), RewriteError> {
    for content in contents.iter() {
        validate_transport_placeholder(content)?;
    }
    if contents.is_empty() {
        return Ok(());
    }
    let token = match sfu.issue_join_token(call_id, peer_identity, capabilities) {
        Ok(token) => {
            record_sfu_token_minted(call_id, correlation, peer_identity);
            token
        }
        Err(error) => {
            record_sfu_token_mint_failure(call_id, correlation, peer_identity, &error);
            return Err(RewriteError::SfuFailed);
        }
    };
    let issued = WaddleLiveKitTransport::Issued(IssuedTransport {
        url: token.url,
        room: token.room,
        identity: token.identity,
        token: token.jwt,
    });
    let issued_elem = issued.to_element();
    for content in contents.iter_mut() {
        content.transport = Some(Transport::Unknown(issued_elem.clone()));
    }
    Ok(())
}

fn record_sfu_token_minted(call_id: &CallId, correlation: &CallCorrelationId, identity: &Identity) {
    let user = identity.as_jid().to_bare();
    tracing::info!(
        room = %call_id.as_str(),
        call.id = %correlation,
        user = %user,
        "LiveKit SFU token minted"
    );
    crate::telemetry::call::increment_sfu_token_minted();
}

fn record_sfu_token_mint_failure(
    call_id: &CallId,
    correlation: &CallCorrelationId,
    identity: &Identity,
    error: &SfuError,
) {
    let user = identity.as_jid().to_bare();
    let reason = SfuDenialReason::InternalError;
    tracing::warn!(
        room = %call_id.as_str(),
        call.id = %correlation,
        user = %user,
        reason = reason.value(),
        error = %error,
        "LiveKit SFU token mint failed"
    );
    crate::telemetry::call::increment_sfu_token_denied(reason);
}

fn record_sfu_token_authorization_denial(room: &BareJid, user: &BareJid) {
    let reason = SfuDenialReason::NotAuthorized;
    tracing::warn!(
        room = %room,
        user = %user,
        reason = reason.value(),
        "SFU token request denied"
    );
    crate::telemetry::call::increment_sfu_token_denied(reason);
}

fn revoke_other_dm_participants(
    sfu: &dyn SfuService,
    call_id: &CallId,
    initiator: &Identity,
    responder: &Identity,
) {
    for participant in sfu.participants_for_call(call_id) {
        if &participant != initiator && &participant != responder {
            let _ = sfu.unregister_call_participant(call_id, &participant, None);
        }
    }
}

fn prune_expired_pending_dm_invites(pending: &mut HashMap<CallId, PendingDmInvite>) {
    let now = Instant::now();
    pending.retain(|_, invite| !invite.is_expired(now));
}

fn iq_set_jingle(iq: &Iq) -> Option<&Element> {
    match iq {
        Iq::Set { payload, .. } if payload.name() == "jingle" && payload.ns() == NS_JINGLE => {
            Some(payload)
        }
        _ => None,
    }
}

/// Empty `<iq type='result'/>` acknowledging a `session-terminate`
/// per XEP-0166 §6.7, emitted server-side when the addressed peer's
/// registration is already gone (survivor terminate, #1131) and there
/// is nobody left to forward the terminate to. `from` mirrors the
/// stanza's `to` so the ack appears from the party the terminate was
/// addressed to, matching the §6.7 example shape.
fn terminate_ack(original: &Iq) -> Vec<OutboundEvent> {
    let ack = Iq::Result {
        from: original.to().cloned(),
        to: original.from().cloned(),
        id: original.id().to_string(),
        payload: None,
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        ack,
    ))))]
}

/// `<item-not-found/>` + `<unknown-session xmlns='urn:xmpp:jingle:errors:1'/>`
/// per the XEP-0166 error table: the stanza references a `sid` with no
/// live session (duplicate terminate, terminate glare, long-dead
/// session). Deliberately NOT `<forbidden/>` (#1131) — nothing was
/// torn down and repeating the request changes nothing.
fn unknown_session_reply(original: &Iq) -> Vec<OutboundEvent> {
    let mut error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "en",
        "unknown Jingle session",
    );
    error.other = Some(crate::xep::xep0166::unknown_session_condition());
    let err = Iq::Error {
        from: original.to().cloned(),
        to: original.from().cloned(),
        id: original.id().to_string(),
        error,
        payload: None,
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        err,
    ))))]
}

fn error_reply(original: &Iq, cond: DefinedCondition, text: &str) -> Vec<OutboundEvent> {
    let err = Iq::Error {
        from: original.to().cloned(),
        to: original.from().cloned(),
        id: original.id().to_string(),
        error: StanzaError::new(ErrorType::Cancel, cond, "en", text),
        payload: None,
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        err,
    ))))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0166;

    /// Pins the 1:1 room-name format through the correlation digest.
    /// The chat twin (`dmCallRoomName` pin in
    /// `chat/tests/call-correlation.test.ts`) asserts the same hex for
    /// the same (initiator, sid), so renaming the format breaks CI on
    /// both sides instead of silently un-joining declined/failed call
    /// lifecycle events from server telemetry (#1452).
    #[test]
    fn dm_room_name_format_digest_is_pinned() {
        let initiator: BareJid = "alice@waddle.test".parse().expect("valid bare jid");
        let call_id = scoped_call_id(&initiator, "c1").expect("valid call id");
        assert_eq!(call_id.as_str(), "alice@waddle.test::c1");
        assert_eq!(
            CallCorrelationId::for_call(&call_id).as_str(),
            "585e23a731089821",
        );
    }

    use crate::xep::xep0167::opus_audio_description;
    use chrono::Duration;
    use jid::FullJid;
    use std::io;
    use waddle_sfu::{
        ApiKey, ApiSecret, LiveKitSfu, SfuConfig, TurnHost, TurnSharedSecret, WebsocketUrl,
    };
    use xmpp_parsers::jingle::{Content, ContentId, Creator, SessionId};

    fn fixture_livekit_sfu() -> Arc<LiveKitSfu> {
        let cfg = SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        };
        Arc::new(LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
    }

    fn fixture_sfu() -> Arc<dyn SfuService> {
        fixture_livekit_sfu()
    }

    fn test_ctx_jid() -> FullJid {
        "alice@waddle.test/desktop".parse().unwrap()
    }

    fn ctx<'a>(jid: &'a FullJid) -> StanzaContext<'a> {
        // Mirrors what the websocket layer's Muji gate supplies for a
        // voiced occupant; tests for the fail-closed path override
        // `media_capabilities` explicitly.
        StanzaContext {
            domain: "waddle.test",
            full_jid: jid,
            media_capabilities: Some(MediaCapabilities::from_muc_voice(
                waddle_xmpp_core::types::Voice::Voiced,
            )),
        }
    }

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn session_initiate_iq(initiator: &str, responder: &str, sid: &str) -> Iq {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId(sid.into()));
        jingle.initiator = Some(initiator.parse().unwrap());
        jingle.contents.push(content);
        Iq::Set {
            from: Some(initiator.parse().unwrap()),
            to: Some(responder.parse().unwrap()),
            id: "i1".into(),
            payload: jingle.into(),
        }
    }

    fn session_accept_iq(responder: &str, to: &str, sid: &str) -> Iq {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionAccept, SessionId(sid.into()));
        jingle.responder = Some(responder.parse().unwrap());
        jingle.contents.push(content);
        Iq::Set {
            from: Some(responder.parse().unwrap()),
            to: Some(to.parse().unwrap()),
            id: "a1".into(),
            payload: jingle.into(),
        }
    }

    fn muji_session_initiate_iq(room: &str) -> Iq {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("muji-observe".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let mut payload: Element = jingle.into();
        payload.append_child(Muji::for_room(room.parse().expect("valid room JID")).to_element());
        Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("calls.waddle.test".parse().unwrap()),
            id: "muji-observe".into(),
            payload,
        }
    }

    fn muji_transport_info_iq(room: &str, sid: &str) -> Iq {
        let mut jingle = Jingle::new(Action::TransportInfo, SessionId(sid.into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        let mut payload: Element = jingle.into();
        payload.append_child(Muji::for_room(room.parse().expect("valid room JID")).to_element());
        Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("calls.waddle.test".parse().unwrap()),
            id: format!("muji-transport-{sid}"),
            payload,
        }
    }

    fn session_terminate_iq(initiator: &str, responder: &str, sid: &str) -> Iq {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId(sid.into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        Iq::Set {
            from: Some(initiator.parse().unwrap()),
            to: Some(responder.parse().unwrap()),
            id: format!("terminate-{sid}"),
            payload: jingle.into(),
        }
    }

    fn register_dm_call(
        sfu: &Arc<LiveKitSfu>,
        initiator: &str,
        responder: &str,
        sid: &str,
    ) -> waddle_sfu::CallId {
        let initiator_bare: BareJid = initiator.parse::<FullJid>().unwrap().to_bare();
        let call = scoped_call_id(&initiator_bare, sid).unwrap();
        sfu.register_call_participant(&call, &Identity::from_jid(initiator.parse().unwrap()));
        sfu.register_call_participant(&call, &Identity::from_jid(responder.parse().unwrap()));
        call
    }

    fn assert_error_condition(events: &[OutboundEvent], condition: DefinedCondition) {
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, condition);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn handler_namespace_is_jingle() {
        assert_eq!(JingleHandler::new(fixture_sfu()).namespace(), NS_JINGLE);
    }

    #[test]
    fn session_initiate_rewrites_empty_transport_with_issued_token() {
        let iq = session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "c1");
        let jid = test_ctx_jid();
        let ctx = ctx(&jid);
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx);

        // Server does NOT pre-ACK; the responder's real client emits
        // the IQ-result. We just forward the rewritten stanza.
        assert_eq!(events.len(), 1, "expected forward only, got {events:?}");
        let OutboundEvent::RouteToConnection {
            jid: peer, stanza, ..
        } = events.into_iter().next().unwrap()
        else {
            panic!("expected RouteToConnection");
        };
        assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
        let Stanza::Iq(fwd) = *stanza else {
            panic!("expected Iq")
        };
        // Forwarded stanza's `from` must be the authenticated session.
        assert_eq!(
            fwd.from().map(|j| j.to_string()),
            Some("alice@waddle.test/desktop".to_string())
        );
        let Iq::Set { payload: elem, .. } = *fwd else {
            panic!("expected Iq set, got error or result")
        };
        let forwarded = Jingle::try_from(elem).expect("forwarded jingle reparses");
        let content = &forwarded.contents[0];
        let Some(Transport::Unknown(t)) = &content.transport else {
            panic!("expected Waddle transport, got other variant")
        };
        let parsed = WaddleLiveKitTransport::try_from(t).expect("transport parses");
        match parsed {
            WaddleLiveKitTransport::Issued(t) => {
                // Room is namespaced by initiator bare JID so an
                // attacker can't pick a sid that collides with a
                // victim's live call.
                assert_eq!(t.room.as_str(), "alice@waddle.test::c1");
                assert_eq!(t.identity.as_livekit_identity(), "bob@waddle.test/desktop");
                assert!(!t.token.as_str().is_empty());
            }
            WaddleLiveKitTransport::Request => panic!("server must populate the transport"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_token_mint_emits_info_with_bare_jid_and_counter() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(
            &muji_session_initiate_iq("general@muc.waddle.test"),
            &ctx(&jid),
        );

        assert!(
            !events.is_empty(),
            "successful Muji token mint must produce the focus response"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.sfu_token.minted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.sfu_token.minted"),
            Some("1".to_string())
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("general@muc.waddle.test"), "{logs}");
        assert!(logs.contains("alice@waddle.test"), "{logs}");
        assert!(logs.contains("\"level\":\"INFO\""), "{logs}");
        assert!(logs.contains("LiveKit SFU token minted"), "{logs}");
        assert!(!logs.contains("alice@waddle.test/desktop"), "{logs}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_spoofed_initiator_emits_not_authorized_denial() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::WARN)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let mut iq = muji_session_initiate_iq("general@muc.waddle.test");
        let Iq::Set { payload, .. } = &mut iq else {
            panic!("expected IQ set");
        };
        payload.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("initiator").to_owned(),
            "mallory@waddle.test/laptop",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::Forbidden);
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.sfu_token.denied",
                &[("reason", "not_authorized")]
            ),
            Some(1)
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("\"level\":\"WARN\""), "{logs}");
        assert!(logs.contains("general@muc.waddle.test"), "{logs}");
        assert!(logs.contains("alice@waddle.test"), "{logs}");
        assert!(!logs.contains("alice@waddle.test/desktop"), "{logs}");
        assert!(!logs.contains("mallory@waddle.test/laptop"), "{logs}");
    }

    #[test]
    fn session_initiate_with_spoofed_initiator_is_rejected() {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("charlie@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "spoof".into(),
            payload: jingle.into(),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn session_initiate_with_same_bare_different_resource_is_rejected() {
        let iq = session_initiate_iq(
            "alice@waddle.test/mobile",
            "bob@waddle.test/desktop",
            "same-bare-spoof",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn session_initiate_with_bare_initiator_is_rejected_on_p2p_path() {
        let iq = session_initiate_iq(
            "alice@waddle.test",
            "bob@waddle.test/desktop",
            "bare-initiator",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn session_accept_with_spoofed_responder_is_rejected() {
        let iq = session_accept_iq(
            "charlie@waddle.test/mobile",
            "alice@waddle.test/desktop",
            "c1",
        );
        let jid: FullJid = "bob@waddle.test/phone".parse().unwrap();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::Forbidden);
    }

    #[test]
    fn session_accept_without_responder_uses_authenticated_sender() {
        let initiate =
            session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/phone", "c1");
        let mut iq = session_accept_iq("bob@waddle.test/phone", "alice@waddle.test/desktop", "c1");
        let Iq::Set { payload, .. } = &mut iq else {
            panic!("expected set");
        };
        let mut jingle = Jingle::try_from(payload.clone()).expect("fixture reparses");
        jingle.responder = None;
        *payload = jingle.into();
        let alice: FullJid = "alice@waddle.test/desktop".parse().unwrap();
        let jid: FullJid = "bob@waddle.test/phone".parse().unwrap();
        let sfu = fixture_livekit_sfu();
        let handler = JingleHandler::new(sfu);
        let initiate_events = handler.handle(&initiate, &ctx(&alice));
        assert!(
            initiate_events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "session-initiate should invite the responder"
        );

        let events = handler.handle(&iq, &ctx(&jid));

        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "missing responder should resolve to authenticated sender and route"
        );
    }

    #[test]
    fn third_party_session_accept_without_invitation_is_rejected() {
        let mut iq = session_accept_iq("eve@waddle.test/laptop", "alice@waddle.test/desktop", "c1");
        let Iq::Set { payload, .. } = &mut iq else {
            panic!("expected set");
        };
        let mut jingle = Jingle::try_from(payload.clone()).expect("fixture reparses");
        jingle.responder = None;
        *payload = jingle.into();
        let eve: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let sfu = fixture_livekit_sfu();
        let call = waddle_sfu::CallId::new("alice@waddle.test::c1").unwrap();
        let handler = JingleHandler::new(sfu.clone());

        let events = handler.handle(&iq, &ctx(&eve));

        assert_error_condition(&events, DefinedCondition::Forbidden);
        assert!(
            !sfu.has_call_participant(&call, &Identity::from_jid(eve)),
            "third-party accept must not register an uninvited participant"
        );
    }

    #[test]
    fn stale_participant_cannot_accept_reused_sid_without_current_invitation() {
        let sfu = fixture_livekit_sfu();
        let call = waddle_sfu::CallId::new("alice@waddle.test::c1").unwrap();
        let eve: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let eve_identity = Identity::from_jid(eve.clone());
        sfu.register_call_participant(&call, &eve_identity);
        assert!(
            sfu.has_call_participant(&call, &eve_identity),
            "fixture starts with Eve as a stale participant for the reused call id"
        );

        let handler = JingleHandler::new(sfu.clone());
        let alice: FullJid = "alice@waddle.test/desktop".parse().unwrap();
        let fresh_invite =
            session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/phone", "c1");
        let initiate_events = handler.handle(&fresh_invite, &ctx(&alice));
        assert!(
            initiate_events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "fresh invite should route to Bob"
        );

        let eve_accept =
            session_accept_iq("eve@waddle.test/laptop", "alice@waddle.test/desktop", "c1");
        let events = handler.handle(&eve_accept, &ctx(&eve));

        assert_error_condition(&events, DefinedCondition::Forbidden);
    }

    #[test]
    fn muji_session_initiate_rejects_bare_initiator() {
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("muji-bare".into()));
        jingle.initiator = Some("alice@waddle.test".parse().unwrap());
        let jid = test_ctx_jid();
        let err = resolve_muji_initiator(&jingle, &ctx(&jid))
            .expect_err("Muji path requires full initiator when present");
        assert!(matches!(err, InitiatorError::InitiatorMismatch));
    }

    #[test]
    fn unsupported_transport_emits_xep_0166_section_10_2_termination() {
        // XEP-0166 §10.2: a session-initiate with an unacceptable
        // transport MUST be rejected with an IQ-result ack followed
        // by a SEPARATE server-initiated session-terminate IQ
        // carrying `<reason><unsupported-transports/></reason>`.
        // Stanza errors like `<feature-not-implemented/>` are
        // NOT the right shape for Jingle negotiation failures
        // with a defined `<reason/>` condition.
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        // ICE-UDP transport — not supported by Waddle.
        content.transport = Some(Transport::Unknown(
            Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1").build(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "i1".into(),
            payload: jingle.into(),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        assert_eq!(
            events.len(),
            2,
            "expected ack + session-terminate per XEP-0166 §10.2"
        );

        // First stanza: empty IQ-result ack.
        let OutboundEvent::SendStanza(ack_stanza) = &events[0] else {
            panic!("expected SendStanza for ack");
        };
        let Stanza::Iq(ack_iq) = ack_stanza.as_ref() else {
            panic!("expected Iq for ack");
        };
        assert!(
            matches!(&**ack_iq, Iq::Result { payload: None, .. }),
            "ack must be an empty IQ result"
        );

        // Second stanza: server-initiated session-terminate.
        let OutboundEvent::SendStanza(term_stanza) = &events[1] else {
            panic!("expected SendStanza for terminate");
        };
        let Stanza::Iq(term_iq) = term_stanza.as_ref() else {
            panic!("expected Iq for terminate");
        };
        let Iq::Set {
            from, to, payload, ..
        } = &**term_iq
        else {
            panic!("expected IQ-set for the session-terminate");
        };
        assert_eq!(
            from.as_ref().map(ToString::to_string),
            Some("bob@waddle.test/desktop".to_string()),
            "unsupported-transport terminate must come from the addressed peer"
        );
        assert_eq!(
            to.as_ref().map(ToString::to_string),
            Some("alice@waddle.test/desktop".to_string()),
            "unsupported-transport terminate must be addressed back to requester"
        );
        let term_jingle =
            Jingle::try_from(payload.clone()).expect("session-terminate Jingle reparses");
        assert_eq!(
            term_jingle.action,
            xmpp_parsers::jingle::Action::SessionTerminate
        );
        let reason = term_jingle
            .reason
            .as_ref()
            .expect("session-terminate must carry a <reason/>");
        assert!(
            matches!(
                reason.reason,
                xmpp_parsers::jingle::Reason::UnsupportedTransports
            ),
            "reason must be <unsupported-transports/> per XEP-0166 §7.4"
        );
    }

    #[test]
    fn session_terminate_routes_to_peer_and_unregisters() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t1".into(),
            payload: jingle.into(),
        };

        let sfu = fixture_livekit_sfu();
        // Pre-register under the scoped id so the unregister path
        // has something to remove.
        let call = waddle_sfu::CallId::new("alice@waddle.test::c1").unwrap();
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        let bob = Identity::from_jid("bob@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        assert_eq!(sfu.participant_count(&call), 2);

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(sfu.clone());
        let events = handler.handle(&iq, &ctx(&jid));
        assert_eq!(sfu.participant_count(&call), 0);

        // No server-forged ACK — just the forwarded terminate.
        assert_eq!(events.len(), 1);
        let mut routed = false;
        for ev in events {
            if let OutboundEvent::RouteToConnection { jid: peer, .. } = ev {
                assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
                routed = true;
            }
        }
        assert!(routed);
    }

    #[test]
    fn session_terminate_does_not_unregister_peer_when_sender_is_not_participant() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("eve@waddle.test/laptop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t-third-party".into(),
            payload: jingle.into(),
        };

        let sfu = fixture_livekit_sfu();
        let call = waddle_sfu::CallId::new("bob@waddle.test::c1").unwrap();
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        let bob = Identity::from_jid("bob@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        assert_eq!(sfu.participant_count(&call), 2);

        let eve: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let handler = JingleHandler::new(sfu.clone());
        let events = handler.handle(&iq, &ctx(&eve));

        assert_eq!(sfu.participant_count(&call), 2);
        assert_error_condition(&events, DefinedCondition::Forbidden);
    }

    #[test]
    fn session_terminate_requires_sender_and_peer_in_same_call() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("eve@waddle.test/laptop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t-cross-call".into(),
            payload: jingle.into(),
        };

        let sfu = fixture_livekit_sfu();
        let eve_call = waddle_sfu::CallId::new("eve@waddle.test::c1").unwrap();
        let bob_call = waddle_sfu::CallId::new("alice@waddle.test::c1").unwrap();
        let eve = Identity::from_jid("eve@waddle.test/laptop".parse().unwrap());
        let mallory = Identity::from_jid("mallory@waddle.test/phone".parse().unwrap());
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        let bob = Identity::from_jid("bob@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&eve_call, &eve);
        sfu.register_call_participant(&eve_call, &mallory);
        sfu.register_call_participant(&bob_call, &alice);
        sfu.register_call_participant(&bob_call, &bob);

        let eve_jid: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let handler = JingleHandler::new(sfu.clone());
        let events = handler.handle(&iq, &ctx(&eve_jid));

        assert_error_condition(&events, DefinedCondition::Forbidden);
        assert_eq!(sfu.participant_count(&eve_call), 2);
        assert_eq!(sfu.participant_count(&bob_call), 2);
    }

    #[test]
    fn session_terminate_to_bare_peer_is_rejected() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test".parse().unwrap()),
            id: "t-bare".into(),
            payload: jingle.into(),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::BadRequest);
    }

    #[test]
    fn malformed_jingle_payload_returns_bad_request() {
        // Right namespace, wrong element name — Jingle::try_from rejects.
        let bogus = Element::builder("garbage", NS_JINGLE).build();
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "m1".into(),
            payload: bogus,
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn bare_to_jid_returns_bad_request() {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().expect("valid full jid")),
            // Bare JID — no resource.
            to: Some("bob@waddle.test".parse().expect("valid jid")),
            id: "b1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn transport_info_forwards_unchanged_with_sanitised_from() {
        let mut jingle = Jingle::new(Action::TransportInfo, SessionId("c1".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        let iq = Iq::Set {
            // Spoofed: client claims `iq.from` is charlie's.
            from: Some(
                "charlie@waddle.test/desktop"
                    .parse()
                    .expect("valid full jid"),
            ),
            to: Some("bob@waddle.test/desktop".parse().expect("valid full jid")),
            id: "ti1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        assert_eq!(events.len(), 1, "no server-forged ACK on transport-info");
        let OutboundEvent::RouteToConnection {
            jid: peer, stanza, ..
        } = events.into_iter().next().unwrap()
        else {
            panic!("expected RouteToConnection");
        };
        assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
        let Stanza::Iq(fwd) = *stanza else { panic!() };
        // route_unchanged must overwrite the spoofed `from`.
        assert_eq!(
            fwd.from().map(|j| j.to_string()),
            Some("alice@waddle.test/desktop".to_string())
        );
    }

    #[test]
    fn cross_domain_jingle_returns_feature_not_implemented() {
        let iq = session_initiate_iq("alice@waddle.test/desktop", "bob@other.test/desktop", "c1");
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = reply.as_ref() else {
                        panic!("expected error")
                    };
                    assert_eq!(
                        err.defined_condition,
                        DefinedCondition::FeatureNotImplemented,
                        "federation is intentionally not supported yet"
                    );
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn same_domain_p2p_jingle_still_passes_federation_guard() {
        // Regression for the relaxed federation guard: a P2P
        // session-initiate between two same-apex-domain accounts
        // must continue to forward to the peer connection without
        // tripping `feature-not-implemented`. Muji equivalents live
        // in the XEP-0272 dedicated test suite at
        // `crates/waddle-xmpp/tests/xep0272_muji.rs` per the
        // CLAUDE.md "XEP custom test-suite hard rule."
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "p2p-1",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        let no_feature_not_implemented = !events.iter().any(|ev| match ev {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => matches!(
                    reply.as_ref(),
                    Iq::Error { error, .. }
                        if error.defined_condition == DefinedCondition::FeatureNotImplemented
                ),
                _ => false,
            },
            _ => false,
        });
        assert!(
            no_feature_not_implemented,
            "same-apex P2P must not trip the federation guard: {events:?}",
        );
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "expected forward to peer, got: {events:?}",
        );
    }

    #[test]
    fn rate_limited_session_initiate_returns_policy_violation() {
        use std::time::Duration;
        // Tight budget: 1 initiate per 30s.
        let rl = Arc::new(SessionInitiateRateLimit::new(1, Duration::from_secs(30)));
        let handler = JingleHandler::with_rate_limit(fixture_sfu(), rl);
        let jid = test_ctx_jid();
        let ctx = ctx(&jid);

        let first = handler.handle(
            &session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "c1"),
            &ctx,
        );
        assert!(
            first
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "first initiate forwards normally"
        );

        // Second initiate within the window must be rejected with
        // policy-violation; no forward, no SFU registration.
        let second = handler.handle(
            &session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "c2"),
            &ctx,
        );
        assert_eq!(second.len(), 1);
        match &second[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error reply, got {reply:?}");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::PolicyViolation);
                }
                other => panic!("expected Iq, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn rate_limited_session_terminate_returns_policy_violation() {
        let sfu = fixture_livekit_sfu();
        register_dm_call(
            &sfu,
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "t1",
        );
        register_dm_call(
            &sfu,
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "t2",
        );
        let handler = JingleHandler::with_rate_limits(
            sfu,
            Arc::new(SessionInitiateRateLimit::with_defaults()),
            Arc::new(TerminateRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
            Arc::new(MujiActionRateLimit::with_defaults()),
        );
        let jid = test_ctx_jid();

        let first = handler.handle(
            &session_terminate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "t1"),
            &ctx(&jid),
        );
        assert!(
            first
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "first terminate should still route"
        );

        let second = handler.handle(
            &session_terminate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "t2"),
            &ctx(&jid),
        );
        assert_error_condition(&second, DefinedCondition::PolicyViolation);
    }

    #[test]
    fn rate_limited_muji_non_initiate_action_returns_policy_violation() {
        let handler = JingleHandler::with_rate_limits(
            fixture_sfu(),
            Arc::new(SessionInitiateRateLimit::with_defaults()),
            Arc::new(TerminateRateLimit::with_defaults()),
            Arc::new(MujiActionRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
        );
        let jid = test_ctx_jid();

        let first = handler.handle(
            &muji_transport_info_iq("general@muc.waddle.test", "muji-ti-1"),
            &ctx(&jid),
        );
        assert_error_condition(&first, DefinedCondition::BadRequest);

        let second = handler.handle(
            &muji_transport_info_iq("general@muc.waddle.test", "muji-ti-2"),
            &ctx(&jid),
        );
        assert_error_condition(&second, DefinedCondition::PolicyViolation);
    }

    #[test]
    fn missing_to_returns_bad_request() {
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle
            .contents
            .push(Content::new(Creator::Initiator, ContentId("audio".into())));
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: None,
            id: "i1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    // ------------------------------------------------------------------
    // #1452 — call setup success-rate counters (`waddle.call.setup.*`).
    //
    // Asserted through the in-memory reader seam, never instrument
    // internals. Every case pins that `attempted` is the true
    // denominator: exactly one terminal `ok`/`failed` per attempt, and
    // non-initiate actions never open one.
    // ------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread")]
    async fn session_initiate_records_attempted_and_defers_ok_to_the_router() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "setup-ok",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        // #1488: the handler no longer counts `ok` at emit time — the
        // route disposition is unknown here. It counts `attempted` and
        // hands the open attempt to the routing interpreter as a
        // ticket on the routing effect.
        assert!(
            matches!(
                events[0],
                OutboundEvent::RouteToConnection {
                    call_setup: Some(_),
                    ..
                }
            ),
            "{events:?}"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.failed", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.setup.attempted"),
            Some("1".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_initiate_ticket_closes_the_attempt_as_ok_when_delivered() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "setup-ticket-ok",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let event = handler
            .handle(&iq, &ctx(&jid))
            .into_iter()
            .next()
            .expect("one event");
        let OutboundEvent::RouteToConnection {
            call_setup: Some(ticket),
            ..
        } = event
        else {
            panic!("expected a routed invite carrying a call-setup ticket: {event:?}");
        };

        ticket.delivered();

        assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
        assert_eq!(
            metrics.metric_unit("waddle.call.setup.ok"),
            Some("1".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_initiate_ticket_closes_the_attempt_as_peer_unavailable() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "setup-ticket-unroutable",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let event = handler
            .handle(&iq, &ctx(&jid))
            .into_iter()
            .next()
            .expect("one event");
        let OutboundEvent::RouteToConnection {
            call_setup: Some(ticket),
            ..
        } = event
        else {
            panic!("expected a routed invite carrying a call-setup ticket: {event:?}");
        };

        ticket.undeliverable();

        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "peer_unavailable")]
            ),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_accept_is_not_counted_as_a_setup_attempt() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let handler = JingleHandler::new(fixture_sfu());
        let jid = test_ctx_jid();
        // Seed the pending invite the accept path requires.
        let initiate = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "accept-not-setup",
        );
        handler.handle(&initiate, &ctx(&jid));

        let bob: FullJid = "bob@waddle.test/desktop".parse().unwrap();
        let accept = session_accept_iq(
            "bob@waddle.test/desktop",
            "alice@waddle.test/desktop",
            "accept-not-setup",
        );
        handler.handle(&accept, &ctx(&bob));

        // Only the initiate counted; the accept added nothing. `ok`
        // stays 0 at the handler seam — the routed initiate defers it
        // to the interpreter (#1488).
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_domain_session_initiate_records_a_federation_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@other.example/desktop",
            "setup-federated",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::FeatureNotImplemented);
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "federation_unsupported")]
            ),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn foreign_transport_session_initiate_records_an_unsupported_transport_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1").build(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("setup-transport".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "i1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        handler.handle(&iq, &ctx(&jid));

        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "unsupported_transport")]
            ),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rate_limited_session_initiate_records_a_rate_limited_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let handler = JingleHandler::with_rate_limit(
            fixture_sfu(),
            Arc::new(SessionInitiateRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
        );
        let jid = test_ctx_jid();
        for sid in ["rl-1", "rl-2"] {
            let iq =
                session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", sid);
            handler.handle(&iq, &ctx(&jid));
        }

        // Two attempts: the first was routed (its `ok`/`failed` close
        // is deferred to the interpreter, #1488), the second was
        // rejected by the limiter — which counts the attempted/failed
        // pair itself because the per-attempt tracker never opens.
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(2)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.failed", &[("reason", "rate_limited")]),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_session_initiate_records_an_attempted_and_ok_setup() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = muji_session_initiate_iq("general@muc.waddle.test");
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_eq!(events.len(), 2, "ack + session-accept: {events:?}");
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_session_initiate_to_a_foreign_room_records_a_bad_request_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = muji_session_initiate_iq("general@muc.other.example");
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::BadRequest);
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.failed", &[("reason", "bad_request")]),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn minted_token_log_carries_the_bounded_call_correlation_id() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let iq = muji_session_initiate_iq("general@muc.waddle.test");
        let jid = test_ctx_jid();
        JingleHandler::new(fixture_sfu()).handle(&iq, &ctx(&jid));

        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        let expected =
            waddle_sfu::CallCorrelationId::for_room_name("general@muc.waddle.test").to_string();
        assert!(logs.contains("LiveKit SFU token minted"), "{logs}");
        assert!(logs.contains(&expected), "{logs}");
        assert_eq!(
            expected.len(),
            waddle_sfu::CORRELATION_ID_HEX_LEN,
            "{expected}"
        );
    }
}
