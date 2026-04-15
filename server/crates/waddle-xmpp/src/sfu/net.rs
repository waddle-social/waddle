//! SFU UDP socket management and media forwarding loop.

use super::SfuRegistry;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tracing::{info, trace, warn};

/// Spawn the SFU UDP event loop as a background tokio task.
///
/// This loop:
/// 1. Reads incoming UDP packets from the shared socket
/// 2. Demuxes them to the correct SfuPeer (via `accepts()`)
/// 3. Polls each peer for outputs (outgoing packets, events)
/// 4. Forwards media data between peers in the same room
///
/// Note: Full media forwarding is not yet implemented.
/// This skeleton establishes the socket binding and receive loop.
pub async fn spawn_sfu_net_loop(
    udp_addr: SocketAddr,
    _registry: Arc<SfuRegistry>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), std::io::Error> {
    let socket = UdpSocket::bind(udp_addr).await?;
    info!(addr = %udp_addr, "SFU UDP socket bound");

    let mut buf = vec![0u8; 2000];

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("SFU net loop shutting down");
                break;
            }
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        trace!(src = %src, len = len, "SFU UDP packet received");
                        // Full demux and forwarding will be added once signaling path is complete
                    }
                    Err(e) => {
                        warn!(error = %e, "SFU UDP recv error");
                    }
                }
            }
        }
    }

    Ok(())
}
