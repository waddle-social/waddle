package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "waddle-server"

let _t = tasks

tasks: {
  build: {
    command: "cargo"
    args: ["build", "--bin", "waddle-server"]
    inputs: [
      "Cargo.toml",
      "Cargo.lock",
      "crates/**",
    ]
  }
  dev: {
    command: "bash"
    args: [
      "-lc",
      """
      set -euo pipefail

      if [ ! -f local/waddle.env ]; then
        echo "Missing server/local/waddle.env"
        exit 1
      fi

      set -a
      . ./local/waddle.env
      set +a

      if printf '%s\n' "${WADDLE_AUTH_PROVIDERS_JSON:-}" | grep -q 'replace-me'; then
        echo "Set the real Colony client secret in server/local/waddle.env before running local auth."
        exit 1
      fi

      cargo run --bin waddle-server
      """,
    ]
    dependsOn: [_t.build]
  }
}
