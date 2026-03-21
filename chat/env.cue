package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "waddle-chat"

let _t = tasks

env: {
  environment: production: {
    CLOUDFLARE_ACCOUNT_ID: schema.#OnePasswordRef & {
      ref: "op://waddle-production/Cloudflare/username"
    }
    CLOUDFLARE_API_TOKEN: schema.#OnePasswordRef & {
      ref: "op://waddle-production/Cloudflare/password"
    }
  }
}

ci: pipelines: {
  default: {
    when: {
      branch:        ["main"]
      defaultBranch: true
      manual:        true
    }
    tasks: [_t.build]
  }
  pullRequest: {
    when: {
      pullRequest: true
    }
    tasks: [_t.build]
  }
}

tasks: {
  install: {
    command: "bun"
    args: ["install", "--frozen-lockfile"]
    inputs: [
      "package.json",
      "bun.lock",
    ]
  }
  generateTypes: {
    command: "bun"
    args: ["run", "generate-types"]
    dependsOn: [_t.install]
    inputs: [
      "package.json",
      "bun.lock",
      "wrangler.jsonc",
    ]
    outputs: [
      "worker-configuration.d.ts",
    ]
  }
  dev: {
    command: "bun"
    args: ["run", "dev"]
    dependsOn: [_t.install]
  }
  build: {
    command: "bun"
    args: ["run", "build"]
    dependsOn: [_t.generateTypes]
    inputs: [
      "package.json",
      "bun.lock",
      "astro.config.ts",
      "tsconfig.json",
      "wrangler.jsonc",
      "src/**",
      "public/**",
    ]
    outputs: [
      "dist/**",
    ]
  }
  deploy: {
    command: "bash"
    args: [
      "-lc",
      """
      set -euo pipefail
      export HOME="${PWD}/.wrangler-home"
      export XDG_CACHE_HOME="${PWD}/.wrangler/cache"
      export XDG_CONFIG_HOME="${PWD}/.wrangler/config"
      export XDG_DATA_HOME="${PWD}/.wrangler/data"
      export WRANGLER_SEND_METRICS="false"
      mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"
      bun run deploy
      """,
    ]
    dependsOn: [_t.build]
    inputs: [
      "wrangler.jsonc",
      "dist/**",
    ]
    outputs: [
      ".wrangler/**",
      ".wrangler-home/**",
    ]
  }
}
