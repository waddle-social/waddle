# Waddle Social

An open-source consumer chat/communication platform with ATProto integration.

## Overview

Waddle Social is a community-focused messaging platform that combines:
- **ATProto Identity**: Login with your Bluesky account
- **XMPP Protocol**: Real-time messaging infrastructure
- **Waddles**: Discord-like communities with channels
- **CLI TUI Client**: Vim-style terminal interface (MVP)

## License

This project is licensed under the **AGPL-3.0** license. See [LICENSE](LICENSE) for details.

## Project Structure

```
waddle/
├── crates/
│   ├── waddle-server/    # Backend HTTP/XMPP server (Axum + Prosody)
│   └── waddle-cli/       # Terminal UI client (Ratatui)
├── docs/
│   ├── adrs/            # Architecture Decision Records
│   ├── rfcs/            # Feature proposals
│   └── specs/           # Technical specifications
└── scripts/             # Development and deployment scripts
```

## Getting Started

### Prerequisites

- Rust 1.75+ (stable)
- A Turso account (for database)
- A Prosody XMPP server (for real-time messaging)

### Development

```bash
# Build all crates
cargo build

# Run the server
cargo run --bin waddle-server

# Run the CLI client
cargo run --bin waddle

# Run tests
cargo test
```

### Container Image

```bash
# Build a local runtime image
docker build -f Containerfile --target runtime -t waddle-server:local .

# Run the server container
docker run --rm -p 3000:3000 -p 5222:5222 -p 5269:5269 waddle-server:local
```

GitHub Actions publishes container images to GHCR on every push to `main` and on semver tags (`vX.Y.Z`).
Release tags publish semver image tags (for example `v0.2.1` -> `0.2.1`, `0.2`, `0`).

## Architecture

Waddle uses a unique architecture combining:

- **Backend**: Rust + Axum for HTTP API
- **Database**: Turso/libSQL with database-per-Waddle sharding
- **Real-time**: Prosody XMPP server for messaging
- **Auth**: ATProto OAuth with DID-based identity
- **Permissions**: Zanzibar-inspired authorization model
- **Actors**: Kameo for concurrent task management

See [docs/adrs/](docs/adrs/) for detailed architectural decisions.

## Documentation

- **[Project Management](docs/PROJECT_MANAGEMENT.md)**: Implementation roadmap and task tracking
- **[Architecture Decisions](docs/adrs/)**: ADRs documenting key technical choices
- **[Feature RFCs](docs/rfcs/)**: Proposals for new features
- **[Technical Specs](docs/specs/)**: Detailed API and protocol specifications
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
