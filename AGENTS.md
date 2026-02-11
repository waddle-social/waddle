# Repository Agent Instructions

## XMPP compliance runs
- Prefer `just` recipes for consistency:
  - `just quick`
  - `just full`
  - `just quick-caas`
- Run compliance via CLI entrypoint: `cargo run --bin waddle -- compliance ...`.
- Quick CAAS-based checks are available via: `cargo run --bin compliance-quick -- --jid <jid> --password <password>`.
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
