//! SfuPeer — wraps a str0m Rtc instance for one participant.

use super::RoomKey;
use crate::xep::xep0176::IceUdpCandidate;
use jid::FullJid;
use std::net::SocketAddr;
use std::time::Instant;
use str0m::change::SdpOffer;
use str0m::media::{MediaKind, Mid};
use str0m::{Candidate, IceConnectionState, Input, Output, Rtc};

/// Convert a Jingle (XEP-0176) `<candidate>` into a str0m `Candidate` by
/// serializing it to the RFC 5245 §15.1 attribute form that str0m parses.
pub fn iceudp_candidate_to_str0m(c: &IceUdpCandidate) -> Result<Candidate, String> {
    let line = format!(
        "candidate:{} {} {} {} {} {} typ {}",
        c.foundation,
        c.component,
        c.protocol,
        c.priority,
        c.ip,
        c.port,
        c.candidate_type.as_str(),
    );
    Candidate::from_sdp_string(&line).map_err(|e| format!("{e:?}"))
}

/// Whether an observed `IceConnectionState` requires tearing down the peer.
///
/// str0m's `IceConnectionState` has no terminal variants by design — per the
/// upstream enum docs, "we never end trickle ice and it's always possible to
/// come back if more remote candidates are added." `Disconnected` in particular
/// is transient and routinely flaps back to `Connected`. Treating any of these
/// states as terminal caused the SFU to kill peers on the first STUN check,
/// breaking every call; str0m manages the `Rtc` lifecycle itself via explicit
/// `disconnect()` calls and DTLS/SCTP failures.
pub fn ice_state_is_terminal(_state: IceConnectionState) -> bool {
    false
}

/// Wraps a single str0m `Rtc` instance representing one SFU participant.
pub struct SfuPeer {
    pub jid: Option<FullJid>,
    pub sid: String,
    pub room_key: RoomKey,
    rtc: Rtc,
    local_addr: SocketAddr,
    /// Negotiated media tracks — (mid, kind) pairs from MediaAdded events.
    pub media_mids: Vec<(Mid, MediaKind)>,
}

impl SfuPeer {
    /// Create a new `SfuPeer` from an incoming SDP offer.
    ///
    /// Builds an ICE-lite `Rtc`, adds a host candidate at `local_addr`,
    /// accepts the SDP offer, and returns `(peer, answer_sdp_string)`.
    pub fn new_from_offer(
        offer_sdp: &str,
        local_addr: SocketAddr,
        room_key: RoomKey,
    ) -> Result<(Self, String), String> {
        let mut rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());

        let candidate = Candidate::host(local_addr, "udp").map_err(|e| format!("{:?}", e))?;
        rtc.add_local_candidate(candidate);

        let offer = SdpOffer::from_sdp_string(offer_sdp).map_err(|e| format!("{:?}", e))?;
        let answer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| format!("{:?}", e))?;
        let answer_sdp = answer.to_sdp_string();

        let peer = Self {
            jid: None,
            sid: String::new(),
            room_key,
            rtc,
            local_addr,
            media_mids: Vec::new(),
        };

        Ok((peer, answer_sdp))
    }

    /// Whether this peer's Rtc accepts the given input.
    pub fn accepts(&self, input: &Input) -> bool {
        self.rtc.accepts(input)
    }

    /// Forward input to the underlying Rtc.
    pub fn handle_input(&mut self, input: Input) -> Result<(), String> {
        self.rtc.handle_input(input).map_err(|e| format!("{:?}", e))
    }

    /// Poll the underlying Rtc for output.
    pub fn poll_output(&mut self) -> Result<Output, String> {
        self.rtc.poll_output().map_err(|e| format!("{:?}", e))
    }

    /// Whether the underlying Rtc session is still alive.
    pub fn is_alive(&self) -> bool {
        self.rtc.is_alive()
    }

    /// Whether the underlying Rtc session is connected.
    pub fn is_connected(&self) -> bool {
        self.rtc.is_connected()
    }

    /// Disconnect the underlying Rtc session.
    pub fn disconnect(&mut self) {
        self.rtc.disconnect();
    }

    /// Register a remote ICE candidate trickled from the client (XEP-0176
    /// `transport-info`). Without this, str0m only knows about whatever
    /// candidates were embedded in the initial SDP offer and cannot match
    /// incoming STUN source addresses to declared candidates.
    pub fn add_remote_candidate(&mut self, c: Candidate) {
        self.rtc.add_remote_candidate(c);
    }

    /// The local address this peer is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Mutable access to the underlying Rtc instance.
    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.rtc
    }

    /// Shared access to the underlying Rtc instance.
    pub fn rtc(&self) -> &Rtc {
        &self.rtc
    }

    /// Test-only constructor that skips SDP negotiation. Used by unit tests
    /// that exercise code paths downstream of peer creation without needing
    /// to fabricate a realistic browser SDP offer.
    #[cfg(test)]
    pub(crate) fn new_for_testing(sid: String, room_key: RoomKey, local_addr: SocketAddr) -> Self {
        let rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());
        Self {
            jid: None,
            sid,
            room_key,
            rtc,
            local_addr,
            media_mids: Vec::new(),
        }
    }

    /// Find the first Mid matching a given media kind (audio/video).
    pub fn mid_for_kind(&self, kind: MediaKind) -> Option<Mid> {
        self.media_mids
            .iter()
            .find(|(_, k)| *k == kind)
            .map(|(mid, _)| *mid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_and_methods_compile() {
        // Verify the struct can be constructed directly (without SDP).
        // A full new_from_offer test requires a realistic browser SDP offer
        // that str0m can parse, which is non-trivial to fabricate.
        let rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());
        let peer = SfuPeer {
            jid: None,
            sid: "test-sid".to_string(),
            room_key: RoomKey("test".to_string()),
            rtc,
            local_addr: "127.0.0.1:9000".parse().expect("valid addr"),
            media_mids: Vec::new(),
        };

        assert!(peer.is_alive());
        assert!(!peer.is_connected());
        assert_eq!(
            peer.local_addr(),
            "127.0.0.1:9000".parse::<SocketAddr>().expect("valid addr")
        );
        assert!(peer.jid.is_none());
        assert_eq!(peer.sid, "test-sid");
    }

    #[test]
    fn disconnect_marks_not_alive() {
        let rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());
        let mut peer = SfuPeer {
            jid: None,
            sid: String::new(),
            room_key: RoomKey("test".to_string()),
            rtc,
            local_addr: "127.0.0.1:9001".parse().expect("valid addr"),
            media_mids: Vec::new(),
        };

        peer.disconnect();
        assert!(!peer.is_alive());
    }

    #[test]
    fn iceudp_host_candidate_round_trips_into_str0m() {
        use crate::xep::xep0176::{CandidateType, IceUdpCandidate};
        let c = IceUdpCandidate::new(
            "1",
            1,
            "udp",
            2113929471,
            "203.0.113.100",
            10100,
            CandidateType::Host,
        );
        let cand = iceudp_candidate_to_str0m(&c).expect("host candidate parses");
        assert_eq!(cand.addr().to_string(), "203.0.113.100:10100");
    }

    #[test]
    fn iceudp_srflx_candidate_round_trips_into_str0m() {
        use crate::xep::xep0176::{CandidateType, IceUdpCandidate};
        let c = IceUdpCandidate::new(
            "2",
            1,
            "udp",
            1677721855,
            "198.51.100.7",
            54321,
            CandidateType::Srflx,
        );
        let cand = iceudp_candidate_to_str0m(&c).expect("srflx candidate parses");
        assert_eq!(cand.addr().to_string(), "198.51.100.7:54321");
    }

    #[test]
    fn iceudp_candidate_rejects_invalid_ip() {
        use crate::xep::xep0176::{CandidateType, IceUdpCandidate};
        let c = IceUdpCandidate::new(
            "1",
            1,
            "udp",
            2113929471,
            "not-an-ip",
            10100,
            CandidateType::Host,
        );
        assert!(
            iceudp_candidate_to_str0m(&c).is_err(),
            "invalid ip must not parse"
        );
    }

    #[test]
    fn no_ice_state_is_terminal() {
        // Regression: killing the peer on IceConnectionState::Disconnected
        // broke every call — str0m treats Disconnected as transient and
        // expects it to resolve back to Connected. See upstream `is-0.8.0`
        // `IceConnectionState` docs: the enum has no terminal variants.
        use str0m::IceConnectionState::*;
        for state in [New, Checking, Connected, Completed, Disconnected] {
            assert!(
                !ice_state_is_terminal(state),
                "ICE state {state:?} must not be terminal — str0m never ends trickle ICE"
            );
        }
    }

    #[test]
    fn rtc_accessors_work() {
        let rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());
        let mut peer = SfuPeer {
            jid: None,
            sid: String::new(),
            room_key: RoomKey("test".to_string()),
            rtc,
            local_addr: "127.0.0.1:9002".parse().expect("valid addr"),
            media_mids: Vec::new(),
        };

        // Just verify the accessors compile and return references
        let _rtc_ref: &Rtc = peer.rtc();
        let _rtc_mut: &mut Rtc = peer.rtc_mut();
    }
}
