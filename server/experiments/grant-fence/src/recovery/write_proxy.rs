//! One-request loopback fault proxy. It never returns an upstream success to
//! the writer. Backend acknowledgement is controller evidence, not a writer fact.
use std::convert::Infallible;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use anyhow::{ensure, Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tonic::codegen::{http, Body, BoxFuture, Service, StdError};
use tonic::transport::{Channel, Endpoint, Server};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::deadline::MonotonicNanos;
use super::model::Identity;
use super::wire_codec::{WireCodec, MAX_MESSAGE_BYTES};
use crate::fixture::Fixture;
use crate::model::{barrier, write_private, Settings, WorkerRequest, BARRIER_BUDGET, RPC_BUDGET};
use crate::process::Process;

const WRITE_PATH: &str = "/authzed.api.v1.PermissionsService/WriteRelationships";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fault {
    DropSuccess,
    NeverForward,
    AfterCallerDeadline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    Listening(u16),
    BackendAcknowledged,
    RequestNotForwarded,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub event: Event,
    pub at: MonotonicNanos,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ProxyWork {
    settings: Settings,
    identity: Identity,
    fault: Fault,
    barriers: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Record {
    identity: Identity,
    observation: Observation,
}
fn record(work: &ProxyWork, name: &str, event: Event) -> Result<()> {
    let temporary = work.barriers.join(format!("{name}.writing"));
    write_private(
        &temporary,
        &serde_json::to_vec(&Record {
            identity: work.identity.clone(),
            observation: Observation {
                event,
                at: MonotonicNanos::now()?,
            },
        })?,
    )?;
    fs::rename(temporary, work.barriers.join(name))?;
    Ok(())
}

#[derive(Clone)]
struct ProxyService {
    work: Arc<ProxyWork>,
    channel: Channel,
    used: Arc<AtomicBool>,
}
impl tonic::server::NamedService for ProxyService {
    const NAME: &'static str = "authzed.api.v1.PermissionsService";
}
impl<B> Service<http::Request<B>> for ProxyService
where
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            if request.uri().path() != WRITE_PATH {
                return Ok(Status::unimplemented("write-only fault proxy").into_http());
            }
            let mut grpc = tonic::server::Grpc::new(WireCodec)
                .max_decoding_message_size(MAX_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_MESSAGE_BYTES);
            Ok(grpc.unary(service, request).await)
        })
    }
}
impl tonic::server::UnaryService<Bytes> for ProxyService {
    type Response = Bytes;
    type Future = BoxFuture<Response<Bytes>, Status>;
    fn call(&mut self, request: Request<Bytes>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            let expected = format!("Bearer {}", service.work.settings.preshared_key);
            if request
                .metadata()
                .get("authorization")
                .map(|value| value.as_bytes())
                != Some(expected.as_bytes())
            {
                return Err(Status::unauthenticated("fixture authorization required"));
            }
            if service.used.swap(true, Ordering::SeqCst) {
                return Err(Status::resource_exhausted("one fault request only"));
            }
            // The one accepted forward operation belongs to this supervised
            // proxy process. Downstream cancellation must not cancel a request
            // already received upstream; the late-commit control exercises it.
            // Every await inside is bounded, and process-group cleanup owns
            // the runtime even when the downstream future is dropped.
            tokio::spawn(service.forward(request))
                .await
                .map_err(|_| Status::internal("owned proxy request task failed"))?
                .map_err(|status| *status)
        })
    }
}
impl ProxyService {
    async fn forward(self, request: Request<Bytes>) -> Result<Response<Bytes>, Box<Status>> {
        if self.work.fault == Fault::AfterCallerDeadline {
            barrier(&self.work.barriers, "forward")
                .await
                .map_err(|_| Status::deadline_exceeded("late forward deadline"))?;
        }
        let observation = match self.work.fault {
            Fault::NeverForward => Event::RequestNotForwarded,
            Fault::DropSuccess | Fault::AfterCallerDeadline => {
                // Tonic checks backend headers, unary body AND final gRPC
                // status before success. The request body is forwarded unchanged.
                let mut upstream = tonic::client::Grpc::new(self.channel)
                    .max_decoding_message_size(MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_MESSAGE_BYTES);
                let response = tokio::time::timeout(RPC_BUDGET, async {
                    upstream
                        .ready()
                        .await
                        .map_err(|_| Status::unavailable("upstream readiness"))?;
                    upstream
                        .unary(
                            request,
                            http::uri::PathAndQuery::from_static(WRITE_PATH),
                            WireCodec,
                        )
                        .await
                })
                .await
                .map_err(|_| Status::deadline_exceeded("upstream deadline"))??;
                if response.get_ref().is_empty() {
                    return Err(Box::new(Status::internal("missing upstream write receipt")));
                }
                Event::BackendAcknowledged
            }
        };
        record(&self.work, "observed", observation)
            .map_err(|_| Status::internal("proxy observation failed"))?;
        barrier(&self.work.barriers, "release")
            .await
            .map_err(|_| Status::deadline_exceeded("fault release deadline"))?;
        Err(Box::new(Status::unavailable(
            "injected unavailable response",
        )))
    }
}

pub async fn execute(work: &ProxyWork) -> Result<()> {
    work.settings.validate()?;
    ensure!(
        work.barriers.starts_with(&work.settings.root),
        "proxy barriers must be fixture-owned"
    );
    let channel = Endpoint::from_shared(format!("http://127.0.0.1:{}", work.settings.grpc_port))?
        .connect_timeout(RPC_BUDGET)
        .timeout(RPC_BUDGET)
        .connect()
        .await?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    record(
        work,
        "ready",
        Event::Listening(listener.local_addr()?.port()),
    )?;
    Server::builder()
        .concurrency_limit_per_connection(1)
        .timeout(BARRIER_BUDGET)
        .add_service(ProxyService {
            work: Arc::new(work.clone()),
            channel,
            used: Arc::new(AtomicBool::new(false)),
        })
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
        .await?;
    Ok(())
}

pub struct Proxy {
    process: Process,
    work: ProxyWork,
    pub port: u16,
}
impl Proxy {
    pub async fn spawn(
        binary: &Path,
        fixture: &Fixture,
        identity: &Identity,
        fault: Fault,
    ) -> Result<Self> {
        let barriers = fixture
            .settings
            .root
            .join(Uuid::new_v4().simple().to_string());
        fs::create_dir(&barriers)?;
        fs::set_permissions(&barriers, fs::Permissions::from_mode(0o700))?;
        let work = ProxyWork {
            settings: fixture.settings.clone(),
            identity: identity.clone(),
            fault,
            barriers,
        };
        let mut command = Command::new(binary);
        command.env_clear();
        let process = Process::spawn_tracked(
            &mut command,
            Some(&serde_json::to_vec(&WorkerRequest::WriteProxy(
                work.clone(),
            ))?),
            false,
            fixture.cleanup_latch(),
            crate::cleanup::CleanupService::Worker,
        )?;
        let mut proxy = Self {
            process,
            work,
            port: 0,
        };
        let Event::Listening(port) = proxy.wait("ready").await?.event else {
            anyhow::bail!("proxy readiness observation");
        };
        ensure!(port != 0, "proxy listener port missing");
        proxy.port = port;
        Ok(proxy)
    }
    async fn read(
        &self,
        name: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Observation>> {
        match tokio::time::timeout_at(deadline, tokio::fs::read(self.work.barriers.join(name)))
            .await
            .context("proxy observation deadline")?
        {
            Ok(bytes) => {
                ensure!(bytes.len() <= 2048, "proxy observation size");
                let record: Record = serde_json::from_slice(&bytes)?;
                ensure!(
                    record.identity == self.work.identity,
                    "proxy observation identity mismatch"
                );
                Ok(Some(record.observation))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub async fn wait(&mut self, name: &str) -> Result<Observation> {
        let deadline = tokio::time::Instant::now() + BARRIER_BUDGET;
        loop {
            if let Some(value) = self.read(name, deadline).await? {
                return Ok(value);
            }
            if self.process.status()?.is_some() {
                return self
                    .read(name, deadline)
                    .await?
                    .context("proxy exited before observation");
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "proxy observation deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    pub async fn assert_rejected_requests(&self) -> Result<()> {
        let channel = Endpoint::from_shared(format!("http://127.0.0.1:{}", self.port))?
            .connect_timeout(RPC_BUDGET)
            .timeout(RPC_BUDGET)
            .connect()
            .await?;
        for (path, payload, authenticated, expected) in [
            (
                WRITE_PATH,
                Bytes::from_static(b"invalid"),
                false,
                tonic::Code::Unauthenticated,
            ),
            (
                "/authzed.api.v1.PermissionsService/ReadRelationships",
                Bytes::new(),
                true,
                tonic::Code::Unimplemented,
            ),
            (
                WRITE_PATH,
                Bytes::from(vec![0; MAX_MESSAGE_BYTES + 1]),
                true,
                tonic::Code::OutOfRange,
            ),
        ] {
            let mut rpc = tonic::client::Grpc::new(channel.clone());
            let mut request = Request::new(payload);
            if authenticated {
                request.metadata_mut().insert(
                    "authorization",
                    format!("Bearer {}", self.work.settings.preshared_key).parse()?,
                );
            }
            rpc.ready().await?;
            // The oversize request uses tonic's bytes encoder without the
            // proxy's local encode cap, exercising the receiving server limit.
            let result: Result<Response<Bytes>, Status> = rpc
                .unary(
                    request,
                    http::uri::PathAndQuery::from_static(path),
                    ProbeCodec,
                )
                .await;
            ensure!(
                matches!(result, Err(status) if status.code() == expected),
                "proxy rejection did not match expected status"
            );
        }
        ensure!(
            self.read("observed", tokio::time::Instant::now() + RPC_BUDGET)
                .await?
                .is_none(),
            "rejected request reached fault boundary"
        );
        Ok(())
    }
    pub fn allow_late_forward(&self) -> Result<()> {
        write_private(&self.work.barriers.join("forward"), b"forward")
    }
    pub fn release(&self) -> Result<()> {
        write_private(&self.work.barriers.join("release"), b"release")
    }
    pub fn stop(&mut self) -> Result<()> {
        self.process.stop()
    }
}

/// Test-only encoder for the single oversized-request boundary probe.
#[derive(Default)]
struct ProbeCodec;
impl tonic::codec::Codec for ProbeCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = Self;
    type Decoder = WireCodec;
    fn encoder(&mut self) -> Self {
        Self
    }
    fn decoder(&mut self) -> WireCodec {
        WireCodec
    }
}
impl tonic::codec::Encoder for ProbeCodec {
    type Item = Bytes;
    type Error = Status;
    fn encode(
        &mut self,
        item: Bytes,
        target: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Status> {
        use bytes::BufMut;
        target.put(item);
        Ok(())
    }
}
