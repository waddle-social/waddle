//! The TLS front of a Waddle deployment's HTTP listener.
//!
//! Waddle's HTTP listener speaks plain HTTP; a deployment terminates TLS
//! in front of it. The front plays that part on loopback, presenting the
//! deployment's `.test` leaf, so clients log in over WSS with the harness
//! CA as their only root.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::pki::{self, Leaf, Pki};

pub struct TlsFront {
    task: JoinHandle<()>,
}

impl TlsFront {
    pub async fn start(pki: &Pki, leaf: &Leaf, port: u16, upstream: u16) -> Result<Self> {
        let config = pki.server_config(leaf, false, &pki::all_versions())?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("bind TLS front on {port}"))?;
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(socket).await else {
                        return;
                    };
                    let Ok(mut plain) = TcpStream::connect(("127.0.0.1", upstream)).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut tls, &mut plain).await;
                });
            }
        });
        Ok(Self { task })
    }
}

impl Drop for TlsFront {
    fn drop(&mut self) {
        self.task.abort();
    }
}
