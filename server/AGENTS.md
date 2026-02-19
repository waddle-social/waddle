# Repository Agent Instructions

## XMPP compliance runs
- Prefer `just` recipes for consistency:
  - `just quick`
  - `just full`
  - `just quick-caas`
  - `just compliance-fast-local`
  - `just compliance-full-local`
- Run compliance via CLI entrypoint: `cargo run --bin waddle -- compliance ...`.
- Quick CAAS-based checks are available via: `cargo run --bin compliance-quick -- --jid <jid> --password <password>`.
- For local iteration with a prebuilt server binary, use:
  - `cargo run --bin waddle -- compliance --skip-server-build`
  - `cargo run --bin waddle -- compliance --server-bin <path-to-waddle-server>`
- CLI compliance runs default to unbounded execution timeout (`WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS=0`) unless overridden in environment.
- Prefer this smoke command shape:
  - `cargo run --bin waddle -- compliance --profile best_effort_full --enabled-specs RFC6120,RFC6121,XEP-0030 --artifact-dir ./test-logs/<run-name>`
- Prefer this quick command shape:
  - `cargo run --bin compliance-quick -- --jid admin@localhost --password pass --artifact-dir ./test-logs/<run-name>`
- `compliance-quick` patches CAAS at runtime to force direct socket connection and bypass SRV-only resolution.
- `compliance-quick` defaults to `--xmpp-host host.docker.internal --xmpp-port 5222` (from inside the CAAS container).
- If your XMPP listener is on a different address/port, pass `--xmpp-host` / `--xmpp-port` explicitly.
- For run-until-completion mode (no interop execution timeout), set:
  - `WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS=0`
- Example: `WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS=0 cargo run --bin waddle -- compliance --profile best_effort_full --enabled-specs RFC6120,RFC6121,XEP-0030 --artifact-dir ./test-logs/<run-name>`
- Compliance percentages are reported from `summary.json` (`interop_progress`) and printed by CLI after run completion.
- `compliance-quick` writes `summary.json`, `caas-stdout.log`, and `caas-stderr.log` to its artifact directory.
- Do not use `./scripts/xmpp-compliance-test.sh` unless explicitly requested.
- Do not run `cargo test -p waddle-xmpp --test xep0479_compliance` directly unless explicitly requested.

## Graceful restart (Ecdysis)

The server implements [Cloudflare's Ecdysis pattern](https://blog.cloudflare.com/ecdysis-rust-graceful-restarts/) for zero-downtime restarts.

### Signal conventions
- `SIGTERM` — Graceful shutdown: stop accepting, drain in-flight connections (30s timeout), exit.
- `SIGQUIT` — Graceful restart: new process starts, old process drains and exits.
- `systemctl reload waddle` sends SIGQUIT (graceful restart).
- `systemctl stop waddle` sends SIGTERM (graceful shutdown).

### Fd inheritance
- On restart, the parent process passes listening sockets to the child via `LISTEN_FDS` / `LISTEN_FD_NAMES` env vars.
- On cold start (no `LISTEN_FDS`), listeners are bound fresh.
- The crate `waddle-ecdysis` handles all fd passing, signal handling, and drain coordination.
- **Unix-only**: `waddle-ecdysis` will not compile on non-Unix platforms.

### State loss on restart
In-memory state is **not** transferred across restarts:
- MUC room presence and rosters
- ISR token store (XEP-0397)
- Stream Management sessions (XEP-0198)
- Connection registry
- PubSub/PEP storage

Connected XMPP clients receive a clean stream close (`</stream:stream>`) during drain and reconnect via XEP-0198 stream resumption. This is acceptable for the current deployment model.

### Configuration
- `WADDLE_DRAIN_TIMEOUT_SECS` — Drain timeout in seconds (default: 30).
