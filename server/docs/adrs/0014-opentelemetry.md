# ADR-0014: OpenTelemetry Instrumentation

## Status

Accepted

## Context

Observability is critical for operating a distributed messaging platform. We need:

1. **Traces**: Follow requests across HTTP and XMPP boundaries
2. **Metrics**: Monitor connection counts, message throughput, latency
3. **Logs**: Structured logging with trace correlation

### Options Considered

| Option | Pros | Cons |
|--------|------|------|
| **OpenTelemetry** | Vendor-neutral, wide ecosystem, traces + metrics + logs | More setup than proprietary SDKs |
| Datadog SDK | Batteries-included | Vendor lock-in, cost |
| Custom metrics | Full control | Reinventing the wheel |
| Prometheus only | Simple, proven | No distributed tracing |

## Decision

Adopt **OpenTelemetry** as the observability foundation from day one, integrated with the `tracing` crate ecosystem.

### Dependencies

```toml
# OpenTelemetry
opentelemetry = "0.28"
opentelemetry_sdk = { version = "0.28", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.28", features = ["tonic", "metrics"] }
tracing-opentelemetry = "0.28"
```

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     waddle-server                           │
├─────────────────────────────────────────────────────────────┤
│  tracing spans (Rust)                                       │
│       │                                                     │
│       ▼                                                     │
│  tracing-opentelemetry                                      │
│       │                                                     │
│       ▼                                                     │
│  opentelemetry SDK                                          │
│       │                                                     │
│       ▼                                                     │
│  OTLP Exporter (gRPC)                                       │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
              ┌─────────────────┐
              │  OTel Collector │  (optional, for routing)
              └─────────────────┘
                        │
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
     ┌─────────┐  ┌──────────┐  ┌──────────┐
     │  Jaeger │  │ Grafana  │  │ Honeycomb│
     │         │  │  Tempo   │  │          │
     └─────────┘  └──────────┘  └──────────┘
```

### Key Spans

Span naming follows OpenTelemetry semantic conventions where applicable.

#### XMPP Server Spans

| Span Name | Attributes | Description |
|-----------|------------|-------------|
| `xmpp.connection.lifecycle` | `jid`, `client_ip`, `transport` | Connection open to close |
| `xmpp.stream.authenticate` | `jid`, `mechanism`, `success` | SASL authentication attempt |
| `xmpp.stanza.process` | `stanza_type`, `from`, `to` | Individual stanza processing |
| `xmpp.muc.join` | `room_jid`, `user_jid`, `affiliation` | MUC room join |
| `xmpp.muc.message` | `room_jid`, `from`, `message_id` | MUC message routing |
| `xmpp.presence.update` | `from`, `show`, `priority` | Presence stanza processing |

#### HTTP Server Spans

| Span Name | Attributes | Description |
|-----------|------------|-------------|
| `http.request` | `method`, `path`, `status_code` | REST API request (tower-http) |
| `http.auth.validate` | `did`, `token_type` | Token validation |
| `db.query` | `query`, `table`, `waddle_id` | Database operations |

### Key Metrics

#### Gauges (Current State)

| Metric | Labels | Description |
|--------|--------|-------------|
| `xmpp.connections.active` | `transport` | Current open connections |
| `xmpp.muc.rooms.active` | `waddle_id` | Active MUC rooms |
| `xmpp.muc.occupants` | `room_jid` | Users in each room |

#### Counters (Cumulative)

| Metric | Labels | Description |
|--------|--------|-------------|
| `xmpp.stanzas.processed` | `type`, `direction` | Total stanzas (message/presence/iq) |
| `xmpp.auth.attempts` | `mechanism`, `result` | Auth attempts (success/failure) |
| `xmpp.muc.messages` | `waddle_id` | Messages sent to MUC rooms |

#### Histograms (Latency)

| Metric | Labels | Buckets (ms) | Description |
|--------|--------|--------------|-------------|
| `xmpp.stanza.latency` | `type` | 1, 5, 10, 25, 50, 100, 250, 500 | Stanza processing time |
| `http.request.duration` | `method`, `path` | 1, 5, 10, 25, 50, 100, 250, 500, 1000 | HTTP request duration |
| `db.query.duration` | `operation` | 1, 5, 10, 25, 50, 100 | Database query time |

### Configuration

Environment variables for OTel configuration:

```bash
# Exporter endpoint
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317

# Service identification
OTEL_SERVICE_NAME=waddle-server
OTEL_SERVICE_VERSION=0.1.0

# Sampling (1.0 = 100%, 0.1 = 10%)
OTEL_TRACES_SAMPLER=parentbased_traceidratio
OTEL_TRACES_SAMPLER_ARG=1.0

# Resource attributes
OTEL_RESOURCE_ATTRIBUTES=deployment.environment=production,service.namespace=waddle
```

### Implementation

#### Telemetry Setup Module

```rust
// src/telemetry.rs
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{runtime, trace as sdktrace, Resource};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_telemetry() -> Result<(), Box<dyn std::error::Error>> {
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                    .unwrap_or_else(|_| "http://localhost:4317".to_string())),
        )
        .with_trace_config(
            sdktrace::Config::default()
                .with_resource(Resource::new(vec![
                    opentelemetry::KeyValue::new("service.name", "waddle-server"),
                ])),
        )
        .install_batch(runtime::Tokio)?;

    let telemetry = tracing_opentelemetry::layer()
        .with_tracer(tracer.tracer("waddle-server"));

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(telemetry)
        .init();

    Ok(())
}

pub fn shutdown_telemetry() {
    opentelemetry::global::shutdown_tracer_provider();
}
```

#### Instrumented XMPP Handler

```rust
use tracing::{instrument, info_span, Instrument};

#[instrument(
    name = "xmpp.stanza.process",
    skip(stanza),
    fields(
        stanza_type = %stanza.name(),
        from = %stanza.from().unwrap_or_default(),
        to = %stanza.to().unwrap_or_default(),
    )
)]
async fn process_stanza(stanza: Stanza) -> Result<(), XmppError> {
    // Processing logic here
}
```

## Consequences

### Positive

- **Vendor Neutral**: Can switch backends (Jaeger → Grafana → Honeycomb) without code changes
- **End-to-End Visibility**: Trace requests from HTTP API through XMPP to database
- **Debugging**: Quickly identify slow queries, auth failures, message delivery issues
- **Capacity Planning**: Metrics inform scaling decisions
- **Standards-Based**: OpenTelemetry is CNCF graduated, wide industry adoption

### Negative

- **Initial Setup**: Requires OTel collector or direct backend configuration
- **Performance Overhead**: Small CPU/memory cost for instrumentation
- **Learning Curve**: Team needs familiarity with OTel concepts

### Mitigations

- **Sampling**: Use trace sampling in production (10-100% based on traffic)
- **Documentation**: Include OTel setup in deployment guide
- **Local Development**: Jaeger all-in-one for easy local debugging

## Development Setup

```yaml
# docker-compose.yml (excerpt)
services:
  jaeger:
    image: jaegertracing/all-in-one:1.54
    ports:
      - "16686:16686"  # UI
      - "4317:4317"    # OTLP gRPC
    environment:
      - COLLECTOR_OTLP_ENABLED=true
```

## Related

- [ADR-0006: Native Rust XMPP Server](./0006-xmpp-protocol.md)
- [ADR-0002: Axum Web Framework](./0002-axum-web-framework.md)
