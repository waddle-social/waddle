use bytes::BytesMut;
use futures::StreamExt;

use super::waddle::extension::types as wit_types;
use crate::host_tools::{HostToolError, HostToolErrorCode};
use crate::types::DisplayText;

#[derive(Debug, Clone)]
pub(super) struct HttpRuntime {
    pub client: reqwest::Client,
    pub permits: std::sync::Arc<tokio::sync::Semaphore>,
}
impl HttpRuntime {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()?,
            permits: std::sync::Arc::new(tokio::sync::Semaphore::new(64)),
        })
    }
}

pub(super) async fn execute_runtime_http_request(
    request: wit_types::OutgoingHttpRequest,
    allowed_origins: &[String],
    runtime: &HttpRuntime,
    limits: &crate::config::RuntimeLimits,
) -> std::result::Result<wit_types::HttpResponse, HostToolError> {
    let url = request.url.value;
    let parsed = reqwest::Url::parse(&url).map_err(|_| {
        HostToolError::invalid_request(
            DisplayText::new("extension HTTP request URL is invalid")
                .expect("static HTTP error is non-empty"),
        )
    })?;
    if parsed.scheme() != "https" {
        return Err(HostToolError::invalid_request(
            DisplayText::new("extension HTTP requests must use https://")
                .expect("static HTTP error is non-empty"),
        ));
    }
    let origin = http_origin(&parsed).ok_or_else(|| {
        HostToolError::invalid_request(
            DisplayText::new("extension HTTP request URL must include a host")
                .expect("static HTTP error is non-empty"),
        )
    })?;
    if !allowed_origins
        .iter()
        .filter_map(|allowed| normalize_http_origin(allowed))
        .any(|allowed| allowed == origin)
    {
        return Err(HostToolError::denied(
            DisplayText::new("extension HTTP origin is not allowed")
                .expect("static HTTP error is non-empty"),
        ));
    }

    let _permit = runtime.permits.acquire().await.map_err(|_| HostToolError {
        code: HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new("extension HTTP capacity unavailable").expect("static error"),
    })?;
    let client = &runtime.client;
    let mut builder = match request.method {
        wit_types::HttpMethod::Get => client.get(&url),
        wit_types::HttpMethod::Post => client.post(&url),
    };
    builder = builder.timeout(std::time::Duration::from_millis(u64::from(
        limits.http_timeout_ms,
    )));
    builder = apply_runtime_http_headers(builder, request.headers)?;
    if let Some(body) = request.body {
        if body.len() > limits.http_max_request_bytes as usize {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP request body is too large")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        builder = builder.body(body);
    }
    let response = builder.send().await.map_err(|_| HostToolError {
        code: HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new("extension HTTP request failed")
            .expect("HTTP error is non-empty"),
    })?;
    let status = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut body = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| HostToolError {
            code: HostToolErrorCode::TemporaryFailure,
            message: DisplayText::new("extension HTTP response body failed")
                .expect("HTTP error is non-empty"),
        })?;
        if body.len() + chunk.len() > limits.http_max_response_bytes as usize {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP response body exceeded limit")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8(body.to_vec()).map_err(|_| HostToolError {
        code: HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new("extension HTTP response body was not UTF-8")
            .expect("HTTP error is non-empty"),
    })?;
    Ok(wit_types::HttpResponse { status, body })
}

pub(super) fn apply_runtime_http_headers(
    mut builder: reqwest::RequestBuilder,
    headers: Vec<wit_types::HttpHeader>,
) -> std::result::Result<reqwest::RequestBuilder, HostToolError> {
    builder = builder.header("accept-encoding", "identity");
    for header in headers {
        if header.name.trim().is_empty() {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP header name must be non-empty")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        if is_disallowed_extension_http_header(&header.name) {
            return Err(HostToolError::invalid_request(
                DisplayText::new("extension HTTP header is controlled by the host")
                    .expect("static HTTP error is non-empty"),
            ));
        }
        builder = builder.header(header.name, header.value);
    }
    Ok(builder)
}

fn http_origin(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let Some(port) = url.port() else {
        return Some(format!("{}://{}", url.scheme(), host));
    };
    Some(format!("{}://{}:{}", url.scheme(), host, port))
}

pub(super) fn normalize_http_origin(value: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(value).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    http_origin(&parsed)
}

pub(super) fn is_disallowed_extension_http_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "te"
            | "trailer"
            | "upgrade"
            | "keep-alive"
            | "accept-encoding"
            | "proxy-authorization"
            | "proxy-authenticate"
    )
}
