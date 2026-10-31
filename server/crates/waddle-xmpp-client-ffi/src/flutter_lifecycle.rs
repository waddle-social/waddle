//! Rust-only shutdown proof for the independent Flutter runtime.
//!
//! Existing UniFFI lifecycle methods and metadata remain unchanged. The Flutter
//! owner serializes connect and this method so a late connect cannot install a
//! handle after shutdown has been acknowledged.

use std::time::Duration;

use crate::{WaddleClient, WaddleError};

impl WaddleClient {
    /// Wait until the driver confirms transport closure, rather than just accepting a command.
    /// This Rust-only helper is deliberately not exported through UniFFI.
    ///
    /// # Errors
    /// Returns `Timeout` without discarding the retained handle if closure cannot be confirmed.
    pub async fn flutter_disconnect_and_wait(&self) -> Result<(), WaddleError> {
        let Some(handle) = self.handle.lock().await.clone() else {
            return Ok(());
        };
        // Admission and session state can both retire before a stalled socket
        // close finishes. Only the driver's final-field lifetime witness proves
        // the transport owner has ended. Bound queue admission as well as exit.
        tokio::time::timeout(Duration::from_secs(12), async {
            let _ = handle.disconnect().await;
            handle.wait_for_driver_exit().await;
        })
        .await
        .map_err(|_| WaddleError::Timeout)?;
        self.handle.lock().await.take();
        Ok(())
    }
}
