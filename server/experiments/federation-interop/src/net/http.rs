//! Minimal HTTP/1.1 on loopback: GET for readiness and Prometheus metrics,
//! and a listener that records every request it receives (the D4 check
//! that no avatar or banner URL is fetched).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A response's status code and body.
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// GET `path` from `127.0.0.1:port`.
pub async fn get(port: u16, path: &str) -> Result<Response> {
    let exchange = async {
        let mut socket = TcpStream::connect(("127.0.0.1", port)).await?;
        let request = [
            "GET ",
            path,
            " HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        ]
        .concat();
        socket.write_all(request.as_bytes()).await?;
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await?;
        anyhow::Ok(bytes)
    };
    let bytes = tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .context("HTTP request timed out")??;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .context("HTTP status line")?;
    Ok(Response {
        status,
        body: body.to_owned(),
    })
}

/// The samples of every metric line whose name starts with `prefix`.
pub fn metric_lines(metrics: &str, prefix: &str) -> Vec<String> {
    metrics
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}

/// The sum of the samples of `name` whose labels contain every pair of
/// `labels`, matched as `key="value"` text.
pub fn metric_sum(metrics: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    metrics
        .lines()
        .filter(|line| {
            line.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
        })
        .filter(|line| {
            labels.iter().all(|(key, value)| {
                let pair = [*key, "=\"", *value, "\""].concat();
                line.contains(&pair)
            })
        })
        .filter_map(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .sum()
}

/// Records the request line of every request; answers 404.
pub struct FetchRecorder {
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl FetchRecorder {
    pub async fn start(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("bind fetch recorder on {port}"))?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let log = Arc::clone(&log);
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 4096];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let text = String::from_utf8_lossy(buffer.get(..read).unwrap_or_default())
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    match log.lock() {
                        Ok(mut log) => log.push(text),
                        Err(poisoned) => poisoned.into_inner().push(text),
                    }
                    let _ = socket
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                        .await;
                });
            }
        });
        Ok(Self { requests, task })
    }

    pub fn requests(&self) -> Vec<String> {
        match self.requests.lock() {
            Ok(requests) => requests.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Drop for FetchRecorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}
