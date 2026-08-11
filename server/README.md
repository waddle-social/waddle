# Waddle Social

An open-source consumer chat/communication platform with ATProto integration.

## Overview

Waddle Social is a community-focused messaging platform that combines:
- **ATProto Identity**: Login with your Bluesky account
- **XMPP Protocol**: Real-time messaging infrastructure
- **Space Channels**: Native XMPP chatrooms backed by server channels

## License

This project is licensed under the **AGPL-3.0** license. See [LICENSE](LICENSE) for details.

## Project Structure

```
waddle/
├── crates/
│   └── waddle-server/    # Backend HTTP/XMPP server
├── docs/
│   ├── adrs/            # Architecture Decision Records
│   ├── rfcs/            # Feature proposals
│   └── specs/           # Technical specifications
└── scripts/             # Development and deployment scripts
```

## Getting Started

### Prerequisites

- Rust 1.75+ (stable)
- SQLite for local development or a PostgreSQL instance for remote testing
- WebSocket-capable clients for XMPP real-time messaging

### Development

```bash
# Build all crates
cargo build

# Run the server
cargo run --bin waddle-server

# Run the CLI client
cargo run --bin waddle

# Run tests
cargo nextest run

# Run doctests (nextest cannot run them)
cargo test --doc
```

### Local Development

```bash
cuenv task dev
```

### Runtime Configuration

- `RUST_LOG`: Standard `tracing` filter expression (takes precedence over `WADDLE_LOG_LEVEL`).
- `WADDLE_LOG_LEVEL`: Optional shorthand log level or full filter for server logging.
  - Shorthand examples: `debug`, `info`, `warn`, `error`
  - Full filter examples: `info,waddle_server=debug,waddle_xmpp=debug`
- Default logging filter (when neither env var is set): `info,waddle_server=debug,waddle_xmpp=debug`
- `WADDLE_XMPP_PUBLIC_WEBSOCKET_URL`: Trusted public RFC 7395 endpoint (`ws://` or `wss://`), the authoritative source for transport-security decisions; when omitted, the server derives a fail-closed `ws://<xmpp-domain>/ws` endpoint.
- `WADDLE_MEDIA_BACKEND`: Media backend selector (`disabled`, `webrtc-rs-sfu`, or `embedded-sfu`).
- `WADDLE_MEDIA_PUBLIC_BASE_URL`: Public base URL used to generate media join endpoints.
- `WADDLE_MEDIA_SFU_SIGNALING_PATH`: Path prefix used for SFU signaling URLs (default `/v1/media/sfu`).
- `WADDLE_MEDIA_SFU_ROOM_PREFIX`: Prefix applied to generated SFU room ids (default `waddle`).
- `WADDLE_MEDIA_SFU_ICE_SERVERS_JSON`: JSON array of ICE/STUN/TURN servers.
- `WADDLE_MEDIA_EMBEDDED_SIGNALING_PATH`: Path prefix for embedded SFU join URLs (default `/v1/media/sfu/embedded`).
- `WADDLE_MEDIA_EMBEDDED_ROOM_PREFIX`: Prefix applied to embedded SFU room ids (default `waddle`).
- `WADDLE_MEDIA_EMBEDDED_MAX_ROOMS`: Hard cap on concurrent embedded SFU rooms (default `128`).
- `WADDLE_MEDIA_EMBEDDED_MAX_PARTICIPANTS_PER_ROOM`: Hard cap per embedded SFU room (default `32`).
- `WADDLE_MEDIA_EMBEDDED_MAX_SESSIONS`: Hard cap on total embedded SFU sessions (default `1024`).
- `WADDLE_SFU_UDP_ADDR`: UDP bind address for XMPP-native SFU media (default `0.0.0.0:10000`).
- `WADDLE_SFU_CANDIDATE_ADDR`: Optional advertised ICE host candidate address (`ip:port`) used by SFU Jingle answers. Set this in production when binding SFU on `0.0.0.0`.
- `WADDLE_CERTS_EPHEMERAL`: Generate ephemeral self-signed TLS certificates in memory at startup (also available as `--ephemeral-certs` CLI flag).
- `WADDLE_TEST_FIXED_ACCOUNT_ENABLED`: Enable boot-time provisioning of a deterministic native XMPP account for integration tests.
- `WADDLE_TEST_FIXED_ACCOUNT_USERNAME`: Native account username (default `admin`).
- `WADDLE_TEST_FIXED_ACCOUNT_PASSWORD`: Native account password. Required when the fixed test account is enabled.
- `WADDLE_TEST_FIXED_ACCOUNT_DOMAIN`: Optional domain override (defaults to `WADDLE_XMPP_DOMAIN`).
- `WADDLE_TEST_FIXED_ACCOUNT_EMAIL`: Optional email for the fixed account.

### Container Image

```bash
# Build a local runtime image
image_stream="$(nix build --print-out-paths ..#waddle-server-image-stream)"
"${image_stream}" | docker image load
docker tag ghcr.io/waddle-social/waddle:nix waddle-server:local

# Run the server container
docker run --rm \
  -p 3000:3000 \
  -e WADDLE_DATABASE_URL=sqlite:///var/lib/waddle/waddle.db \
  -e WADDLE_XMPP_MAM_DATABASE_URL=sqlite:///var/lib/waddle/mam.db \
  -e WADDLE_XMPP_INBOX_DATABASE_URL=sqlite:///var/lib/waddle/inbox.db \
  -e WADDLE_SESSION_KEY="$(openssl rand -base64 48)" \
  -e WADDLE_OCCUPANT_ID_SECRET="$(openssl rand -base64 48)" \
  -e WADDLE_DEPLOYMENT_UUID="$(uuidgen | tr 'A-Z' 'a-z')" \
  -e WADDLE_DB_LINEAGE_ACTION=enroll \
  waddle-server:local
```

`WADDLE_DEPLOYMENT_UUID` and the one-shot `WADDLE_DB_LINEAGE_ACTION=enroll`
enroll the durable SQLite files' database lineage on first run (#1652);
keep the UUID stable (store it next to your secrets) and drop the enroll
action after the first successful start. Purely in-memory runs need
neither. See `docs/operations/db-lineage.md`.

`WADDLE_SESSION_KEY` is required for session token HMAC and extension launch
signing. Keep it stable across restarts so existing sessions and extension
launches remain valid.

`WADDLE_OCCUPANT_ID_SECRET` is the per-deployment HMAC key used to derive
XEP-0421 occupant identifiers. It must be at least 32 bytes and stay
stable across restarts; rotating it severs occupant-id continuity for
clients tracking users across nick changes. The Helm chart does not generate
runtime secrets; deployments must supply both runtime keys through their
secret manager.

GitHub Actions publishes container images to GHCR on every push to `main` and on semver tags (`vX.Y.Z`).
Release tags publish semver image tags (for example `v0.2.1` -> `0.2.1`, `0.2`, `0`).

### Kubernetes (Helm)

An in-repo Helm chart is available at `charts/waddle-server`.

```bash
helm upgrade --install waddle ./charts/waddle-server --namespace waddle --create-namespace
```

## Architecture

Waddle uses a unique architecture combining:

- **Backend**: Rust + Axum for HTTP API
- **Database**: SQLx-backed adapters for SQLite and PostgreSQL with a single logical Waddle database
- **Real-time**: Embedded WebSocket XMPP C2S server for messaging
- **Auth**: ATProto OAuth with DID-based identity
- **Permissions**: Zanzibar-inspired authorization model
- **Actors**: Kameo for concurrent task management

See [docs/adrs/](docs/adrs/) for detailed architectural decisions.

## Documentation

- **[Project Management](docs/PROJECT_MANAGEMENT.md)**: Implementation roadmap and task tracking
- **[Architecture Decisions](docs/adrs/)**: ADRs documenting key technical choices
- **[Feature RFCs](docs/rfcs/)**: Proposals for new features
- **[Technical Specs](docs/specs/)**: Detailed API and protocol specifications
- **[Database Lineage Runbook](docs/operations/db-lineage.md)**: Enrollment, adoption, and readiness attestation for durable database deployments
- **[Call Stack Operator Runbook](docs/specs/call-stack-operator-runbook.md)**: Rollout, verification, and troubleshooting for media/call operations
- **[Rust Crates](docs/RUST_CRATES.md)**: Recommended dependencies

## MVP Milestones

### M1: Hello Waddle (Current)
- [ ] User authentication via Bluesky (ATProto OAuth)
- [ ] XMPP account provisioning from DID
- [ ] Create and manage Waddles
- [ ] Create channels (XMPP MUC rooms)
- [ ] Send/receive messages in CLI
- [ ] Real-time message delivery

### M2: Rich Messaging
- [ ] File uploads (XEP-0363)
- [ ] XHTML-IM formatting
- [ ] Reactions and replies
- [ ] Direct messages
- [ ] Presence indicators

## Contributing

Contributions are welcome! Please read our [Code of Conduct](CODE_OF_CONDUCT.md) first.

### Development Workflow

1. Check [docs/PROJECT_MANAGEMENT.md](docs/PROJECT_MANAGEMENT.md) for tasks
2. Read relevant ADRs in [docs/adrs/](docs/adrs/)
3. Implement with tests
4. Update documentation as needed
5. Submit a pull request

## Community

- **GitHub Issues**: Bug reports and feature requests
- **Discussions**: Questions and ideas

## Status

🚧 **Early Development** - MVP in progress

This project is in active development. APIs and features are subject to change.

---

Built with ❤️ by the Waddle community

<!-- CI baseline probe for #1627: comment-only change to trigger server path filters. -->
