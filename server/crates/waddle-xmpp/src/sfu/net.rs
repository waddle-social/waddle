//! SFU UDP socket management and media forwarding loop.
//!
//! Two-pass architecture (following str0m's chat example pattern):
//! 1. Poll all peers for outputs, send UDP transmits immediately,
//!    collect media events into a queue.
//! 2. Forward collected media data to other peers in the same room.

use super::{PeerStore, RoomKey};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::media::{KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Event, IceConnectionState, Input, Output};
use tokio::net::UdpSocket;
use tracing::{debug, info, trace, warn};

/// Events collected during polling that need to be forwarded to other peers.
enum ForwardEvent {
    /// A new media track was detected on a peer.
    MediaAdded {
        sid: String,
        mid: Mid,
        kind: MediaKind,
    },
    /// Media data received from a peer — forward to all others in the room.
    ///
    /// `data` is boxed because `MediaData` is ~500 bytes and would otherwise
    /// force every `ForwardEvent` variant to pay that cost via the enum tag
    /// alignment (see `clippy::large_enum_variant`).
    Media {
        source_sid: String,
        room_key: RoomKey,
        source_kind: MediaKind,
        data: Box<MediaData>,
    },
    /// A peer requested a keyframe — forward to the source of that track.
    Keyframe {
        source_sid: String,
        room_key: RoomKey,
        req: KeyframeRequest,
    },
}

/// Spawn the SFU UDP event loop as a background tokio task.
///
/// This loop:
/// 1. Reads incoming UDP packets from the shared socket
/// 2. Demuxes them to the correct SfuPeer (via `accepts()`)
/// 3. Polls each peer for outputs (outgoing packets, events)
/// 4. Forwards media data between peers in the same room
/// 5. Removes dead peers
pub async fn spawn_sfu_net_loop(
    udp_addr: SocketAddr,
    peer_store: Arc<PeerStore>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), std::io::Error> {
    let socket = UdpSocket::bind(udp_addr).await?;
    let local_addr = socket.local_addr()?;
    info!(addr = %local_addr, "SFU UDP socket bound");

    let mut buf = vec![0u8; 2000];
    let mut poll_interval = tokio::time::interval(Duration::from_millis(20));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("SFU net loop shutting down");
                break;
            }

            // Branch 1: receive UDP packet and demux to correct peer
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, source)) => {
                        trace!(src = %source, len = len, "SFU UDP packet received");

                        let now = Instant::now();
                        let receive = match Receive::new(
                            Protocol::Udp,
                            source,
                            local_addr,
                            &buf[..len],
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                trace!(error = %e, "Failed to parse incoming datagram");
                                continue;
                            }
                        };

                        let input = Input::Receive(now, receive);

                        let mut peers = peer_store.peers().write().await;
                        let mut matched = false;
                        for peer in peers.values_mut() {
                            if peer.accepts(&input) {
                                if let Err(e) = peer.handle_input(input) {
                                    warn!(sid = %peer.sid, error = %e, "Peer failed to handle input");
                                }
                                matched = true;
                                break;
                            }
                        }

                        if !matched {
                            trace!(src = %source, "No peer accepted incoming UDP packet");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "SFU UDP recv error");
                    }
                }
            }

            // Branch 2: periodic poll — two-pass: poll events, then forward media
            _ = poll_interval.tick() => {
                let now = Instant::now();
                let mut peers = peer_store.peers().write().await;
                let mut forward_queue: Vec<ForwardEvent> = Vec::new();
                let mut dead_sids: Vec<String> = Vec::new();

                // === PASS 1: Poll all peers, send transmits, collect media events ===
                let sids: Vec<String> = peers.keys().cloned().collect();
                for sid in &sids {
                    let Some(peer) = peers.get_mut(sid) else {
                        continue;
                    };

                    if let Err(e) = peer.handle_input(Input::Timeout(now)) {
                        warn!(sid = %sid, error = %e, "Peer timeout input failed");
                        continue;
                    }

                    let peer_room_key = peer.room_key.clone();

                    loop {
                        match peer.poll_output() {
                            Ok(Output::Transmit(t)) => {
                                let dest = t.destination;
                                let data: Vec<u8> = t.contents.into();
                                match socket.try_send_to(&data, dest) {
                                    Ok(_) => {}
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        trace!(dest = %dest, "SFU UDP send would block");
                                    }
                                    Err(e) => {
                                        warn!(dest = %dest, error = %e, "SFU UDP send error");
                                    }
                                }
                            }
                            Ok(Output::Event(event)) => {
                                match event {
                                    Event::Connected => {
                                        info!(sid = %sid, "SFU peer connected");
                                    }
                                    Event::IceConnectionStateChange(state) => {
                                        debug!(sid = %sid, state = ?state, "SFU peer ICE state changed");
                                        if state == IceConnectionState::Disconnected {
                                            peer.disconnect();
                                        }
                                    }
                                    Event::MediaAdded(added) => {
                                        debug!(
                                            sid = %sid,
                                            mid = ?added.mid,
                                            kind = ?added.kind,
                                            "Media track added on peer"
                                        );
                                        forward_queue.push(ForwardEvent::MediaAdded {
                                            sid: sid.clone(),
                                            mid: added.mid,
                                            kind: added.kind,
                                        });
                                    }
                                    Event::MediaData(data) => {
                                        // Look up media kind from peer's tracked mids
                                        let kind = peer.media_mids.iter()
                                            .find(|(m, _)| *m == data.mid)
                                            .map(|(_, k)| *k)
                                            .unwrap_or(MediaKind::Audio);

                                        if !data.contiguous {
                                            // Request keyframe when there's a gap
                                            if let Some(mut writer) = peer.rtc_mut().writer(data.mid) {
                                                let _ = writer.request_keyframe(
                                                    data.rid,
                                                    KeyframeRequestKind::Fir,
                                                );
                                            }
                                        }

                                        forward_queue.push(ForwardEvent::Media {
                                            source_sid: sid.clone(),
                                            room_key: peer_room_key.clone(),
                                            source_kind: kind,
                                            data: Box::new(data),
                                        });
                                    }
                                    Event::KeyframeRequest(req) => {
                                        forward_queue.push(ForwardEvent::Keyframe {
                                            source_sid: sid.clone(),
                                            room_key: peer_room_key.clone(),
                                            req,
                                        });
                                    }
                                    _ => {
                                        trace!(sid = %sid, event = ?event, "SFU peer event");
                                    }
                                }
                            }
                            Ok(Output::Timeout(_)) => break,
                            Err(e) => {
                                warn!(sid = %sid, error = %e, "Peer poll_output failed");
                                break;
                            }
                        }
                    }

                    let Some(peer) = peers.get(sid) else { continue };
                    if !peer.is_alive() {
                        dead_sids.push(sid.clone());
                    }
                }

                // === PASS 2: Process forward queue ===
                for event in &forward_queue {
                    match event {
                        ForwardEvent::MediaAdded { sid, mid, kind } => {
                            // Record the mid/kind on the peer
                            if let Some(peer) = peers.get_mut(sid) {
                                if !peer.media_mids.iter().any(|(m, _)| *m == *mid) {
                                    peer.media_mids.push((*mid, *kind));
                                }
                            }
                        }
                        ForwardEvent::Media { source_sid, room_key, source_kind, data } => {
                            // Forward to all OTHER peers in the same room
                            let target_sids: Vec<String> = peers.keys()
                                .filter(|s| *s != source_sid)
                                .cloned()
                                .collect();

                            for target_sid in &target_sids {
                                let Some(target) = peers.get_mut(target_sid) else { continue };
                                if target.room_key != *room_key { continue; }

                                // Find matching Mid on target by media kind
                                let target_mid = target.mid_for_kind(*source_kind);
                                let Some(mid) = target_mid else { continue };

                                let Some(writer) = target.rtc_mut().writer(mid) else { continue };
                                let Some(pt) = writer.match_params(data.params) else { continue };

                                if let Err(e) = writer.write(
                                    pt,
                                    data.network_time,
                                    data.time,
                                    data.data.clone(),
                                ) {
                                    warn!(
                                        source = %source_sid,
                                        target = %target_sid,
                                        error = %e,
                                        "Failed to forward media"
                                    );
                                }
                            }
                        }
                        ForwardEvent::Keyframe { source_sid, room_key, req } => {
                            // The requesting peer wants a keyframe. Forward the request
                            // to all OTHER peers in the room (the senders).
                            let target_sids: Vec<String> = peers.keys()
                                .filter(|s| *s != source_sid)
                                .cloned()
                                .collect();

                            for target_sid in &target_sids {
                                let Some(target) = peers.get_mut(target_sid) else { continue };
                                if target.room_key != *room_key { continue; }

                                // Find the corresponding incoming mid on the target
                                // (the target is the one SENDING media that the source wants a keyframe for)
                                let target_mid = target.media_mids.iter()
                                    .find(|(_, k)| {
                                        // Match by media kind — keyframes are for video
                                        *k == MediaKind::Video
                                    })
                                    .map(|(m, _)| *m);

                                if let Some(mid) = target_mid {
                                    if let Some(mut writer) = target.rtc_mut().writer(mid) {
                                        let _ = writer.request_keyframe(req.rid, req.kind);
                                    }
                                }
                            }
                        }
                    }
                }

                // Remove dead peers
                for sid in &dead_sids {
                    info!(sid = %sid, "Removing dead SFU peer");
                    peers.remove(sid);
                }
            }
        }
    }

    Ok(())
}
