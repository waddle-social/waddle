//! XEP-0272 Muji group-call exports: the three verbs the wasm chat
//! client drives for SFU-mediated group calls, ported byte-for-byte
//! onto the shared `waddle-xmpp-client` builders.
//!
//! Waddle's divergence from vanilla XEP-0272 (sanctioned by its
//! §"Relays and Mixers"): ONE Jingle session to the SFU mixer
//! component `calls.<server-domain>` instead of a full mesh, with the
//! custom `urn:waddle:transports:livekit:0` transport the server
//! rewrites into LiveKit join credentials. The presence protocol
//! (preparing → contents → bare-presence leave) stays XEP-conformant.
//!
//! The mixer's `session-accept` (XEP-0166 §6.3 separate IQ-set,
//! stamped with the XEP-0298 `isfocus` marker) is parsed by the shared
//! Jingle parser and surfaces through `WaddleClientEvent::Call` as a
//! typed `SessionAccept` — there is no Muji-specific accept parser;
//! callers correlate by the per-attempt sid.

use minidom::Element;

use jid::FullJid;
use waddle_xmpp_client::caps::build_client_caps_element;
use waddle_xmpp_client::messaging::{
    build_in_call_presence_state_element, build_muji_element, build_muji_session_initiate,
    build_muji_session_terminate, SessionId, NS_CLIENT,
};

use crate::boundary_convert::jid_domain;
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::stanza::iq_set;
use crate::{WaddleClient, WaddleError};

/// Waddle SFU mixer component JID for the user's own server
/// (wasm `calls_mixer_jid` parity).
pub(crate) fn muji_mixer_jid(server_domain: &str) -> String {
    format!("calls.{server_domain}")
}

/// Typed mixer target derived from the configured ACCOUNT JID: any
/// resource is stripped BEFORE the domain split (a full JID would leak
/// `domain/resource` into the mixer host), and a malformed result
/// surfaces as [`WaddleError::InvalidJid`] instead of a panic.
pub(crate) fn muji_mixer_target(account_jid: &str) -> Result<jid::Jid, WaddleError> {
    let bare = account_jid.split('/').next().unwrap_or(account_jid);
    muji_mixer_jid(jid_domain(bare))
        .parse()
        .map_err(|_| WaddleError::InvalidJid)
}

/// `urn:waddle:in-call:0` presence flags carried ALONGSIDE `<muji/>`
/// (never inside it): raised hand and self-reported mute. Presence is
/// last-writer-wins, so every update re-stamps BOTH flags.
#[derive(uniffi::Record, Clone, Copy, Debug, Default)]
pub struct WaddleInCallPresenceFlags {
    pub hand_raised: bool,
    pub muted: bool,
}

/// Pure factory: the Muji-bearing MUC presence (wasm
/// `update_muji_presence` wire shape). `preparing=true` appends the
/// XEP-0272 §Joining `<preparing/>` marker; `active=true` declares the
/// audio content (audio is implied by being in a call) plus video when
/// requested; neither flag set yields the bare presence that is the
/// XEP-0272 §Leaving marker. The XEP-0115 caps element rides every
/// variant.
pub(crate) fn muji_presence_stanza(
    room_jid: &str,
    nick: &str,
    active: bool,
    preparing: bool,
    video: bool,
    flags: WaddleInCallPresenceFlags,
) -> Element {
    let to = format!("{room_jid}/{nick}");
    let mut builder = Element::builder("presence", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str());
    if preparing || active {
        builder = builder.append(build_muji_element(preparing, active, active && video));
        if flags.hand_raised || flags.muted {
            builder = builder.append(build_in_call_presence_state_element(
                flags.hand_raised,
                flags.muted,
            ));
        }
    }
    builder.append(build_client_caps_element()).build()
}

/// Pure factory: the mixer-addressed `session-initiate` IQ (wasm
/// parity — `<muji room=…/>` first, then audio content, plus video
/// content when requested, each with the empty LiveKit transport
/// placeholder the server rewrites). Takes the pre-parsed mixer JID
/// ([`muji_mixer_target`]) so no parse can fail here.
pub(crate) fn muji_session_initiate_iq(
    mixer: &jid::Jid,
    sid: &SessionId,
    initiator: &FullJid,
    room_jid: &str,
    video: bool,
) -> Element {
    iq_set(
        mixer,
        build_muji_session_initiate(sid, initiator, room_jid, video),
    )
}

/// Pure factory: the mixer-addressed `session-terminate` IQ
/// (`<muji room=…/>` + `<reason><success/></reason>`).
pub(crate) fn muji_session_terminate_iq(
    mixer: &jid::Jid,
    room_jid: &str,
    sid: &SessionId,
) -> Element {
    iq_set(mixer, build_muji_session_terminate(room_jid, sid))
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0272 over Waddle's SFU: send the Muji-bearing Jingle
    /// `session-initiate` to `calls.<own-domain>`. Resolves on the
    /// EMPTY IQ-result ack (XEP-0166 §6.3); the actual
    /// `session-accept` with the rewritten LiveKit transport arrives
    /// as a separate server-initiated IQ-set through
    /// `WaddleClientEvent::Call`, correlated by `sid` (a per-attempt
    /// id minted by the caller, so a stale accept from a cancelled
    /// same-room retry is ignorable without changing room semantics).
    /// `initiator_full_jid` MUST be the bound resource — the server
    /// rejects a mismatch with the authenticated session.
    pub async fn send_muji_session_initiate(
        &self,
        room_jid: String,
        initiator_full_jid: String,
        sid: String,
        video: bool,
    ) -> Result<(), WaddleError> {
        let initiator: FullJid = initiator_full_jid
            .parse()
            .map_err(|_| WaddleError::InvalidJid)?;
        let room = self.require_bare_jid(&room_jid)?;
        let sid = require_session_id(sid)?;
        let mixer = muji_mixer_target(&self.config.jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = muji_session_initiate_iq(&mixer, &sid, &initiator, room.as_str(), video);
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }

    /// XEP-0272 leave, mixer half: `session-terminate` with
    /// `<reason><success/></reason>` to `calls.<own-domain>`. The
    /// server unregisters the participant and revokes every LiveKit
    /// token it minted for `(room, identity)`; the call is idempotent.
    /// Per XEP-0272 §Leaving, the bare MUC presence
    /// ([`Self::update_muji_presence`] with neither flag) MUST go out
    /// first.
    pub async fn send_muji_session_terminate(
        &self,
        room_jid: String,
        sid: String,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let sid = require_session_id(sid)?;
        let mixer = muji_mixer_target(&self.config.jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = muji_session_terminate_iq(&mixer, room.as_str(), &sid);
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }

    /// Publish this occupant's Muji call presence to the room
    /// (XEP-0272 §Joining / §Leaving): `preparing=true` for the
    /// two-phase setup marker, `active=true` for the content-declaring
    /// join (audio implied, video on request), neither for the
    /// bare-presence leave marker. `flags` re-stamps the
    /// `urn:waddle:in-call:0` raised-hand/mute markers alongside
    /// `<muji/>` — meaningless without call participation, so they are
    /// dropped on the leave variant.
    pub async fn update_muji_presence(
        &self,
        room_jid: String,
        nick: String,
        active: bool,
        preparing: bool,
        video: bool,
        flags: WaddleInCallPresenceFlags,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        if nick.trim().is_empty() {
            return Err(WaddleError::InvalidJid);
        }
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let presence = muji_presence_stanza(room.as_str(), &nick, active, preparing, video, flags);
        handle
            .send_stanza(presence)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }
}

fn require_session_id(value: String) -> Result<SessionId, WaddleError> {
    if value.trim().is_empty() {
        return Err(WaddleError::InvalidSessionId);
    }
    Ok(SessionId(value))
}
