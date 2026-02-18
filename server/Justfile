set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Managed harness quick run (starts/stops server automatically inside harness).
quick artifact_dir="test-logs/quick-managed" enabled_specs="RFC6120,RFC6121,XEP-0030":
    WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS=0 cargo run --bin waddle -- compliance --profile best_effort_full --enabled-specs {{enabled_specs}} --artifact-dir {{artifact_dir}}

# Managed harness full run (starts/stops server automatically inside harness).
full artifact_dir="test-logs/full-managed":
    WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS=0 cargo run --bin waddle -- compliance --profile best_effort_full --artifact-dir {{artifact_dir}}

# CAAS quick run with local server started/stopped by this recipe.
quick-caas artifact_dir="test-logs/quick-caas" jid="admin@localhost" password="pass" xmpp_host="host.docker.internal" xmpp_port="5222":
    #!/usr/bin/env bash
    set -euo pipefail

    artifact_dir="{{artifact_dir}}"
    jid="{{jid}}"
    password="{{password}}"
    xmpp_host="{{xmpp_host}}"
    xmpp_port="{{xmpp_port}}"
    http_port="3000"

    if (echo > /dev/tcp/127.0.0.1/"$xmpp_port") >/dev/null 2>&1; then
      echo "Cannot run quick-caas: XMPP port 127.0.0.1:${xmpp_port} is already in use."
      echo "Stop the conflicting process or run with a different xmpp_port."
      exit 1
    fi

    if (echo > /dev/tcp/127.0.0.1/"$http_port") >/dev/null 2>&1; then
      echo "Cannot run quick-caas: HTTP port 127.0.0.1:${http_port} is already in use."
      echo "Stop the conflicting process before running quick-caas."
      exit 1
    fi

    mkdir -p "$artifact_dir"
    server_log="$artifact_dir/waddle-server.log"
    db_path="$artifact_dir/quick-caas.db"
    rm -rf "$db_path"

    echo "Starting local waddle-server for quick-caas..."
    cargo build -p waddle-server --bin waddle-server

    WADDLE_MODE=standalone \
    WADDLE_BASE_URL="http://host.docker.internal:${http_port}" \
    WADDLE_DB_PATH="$db_path" \
    WADDLE_XMPP_ENABLED=true \
    WADDLE_XMPP_DOMAIN=localhost \
    WADDLE_XMPP_C2S_ADDR="0.0.0.0:${xmpp_port}" \
    WADDLE_XMPP_TLS_CERT=certs/server.crt \
    WADDLE_XMPP_TLS_KEY=certs/server.key \
    WADDLE_XMPP_S2S_ENABLED=false \
    WADDLE_NATIVE_AUTH_ENABLED=true \
    WADDLE_XMPP_ISR_IN_SASL_SUCCESS=false \
    WADDLE_REGISTRATION_ENABLED=true \
    target/debug/waddle-server >"$server_log" 2>&1 &
    server_pid=$!

    cleanup() {
      kill "$server_pid" 2>/dev/null || true
      wait "$server_pid" 2>/dev/null || true
    }
    trap cleanup EXIT

    ready=0
    for _ in $(seq 1 45); do
      if ! kill -0 "$server_pid" >/dev/null 2>&1; then
        echo "waddle-server exited before readiness check completed."
        echo "Likely cause: a bind conflict on XMPP or HTTP ports."
        echo "See: $server_log"
        exit 1
      fi
      if grep -q "Address already in use" "$server_log"; then
        echo "waddle-server failed to bind 127.0.0.1:${xmpp_port} (address already in use)."
        echo "See: $server_log"
        exit 1
      fi
      if (echo > /dev/tcp/127.0.0.1/"$xmpp_port") >/dev/null 2>&1; then
        ready=1
        break
      fi
      sleep 1
    done

    if [[ "$ready" -ne 1 ]]; then
      echo "waddle-server did not open 127.0.0.1:${xmpp_port} in time."
      echo "See: $server_log"
      exit 1
    fi

    sleep 1
    if ! kill -0 "$server_pid" >/dev/null 2>&1; then
      echo "waddle-server exited right after opening 127.0.0.1:${xmpp_port}."
      echo "See: $server_log"
      exit 1
    fi

    cargo run --bin compliance-quick -- \
      --jid "$jid" \
      --password "$password" \
      --artifact-dir "$artifact_dir" \
      --xmpp-host "$xmpp_host" \
      --xmpp-port "$xmpp_port" \
      --host-meta-base-url "http://${xmpp_host}:${http_port}" \
      --xep0368-starttls-fallback
