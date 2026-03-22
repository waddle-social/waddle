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
    environment: "production"
    when: {
      branch:        ["main"]
      defaultBranch: true
      manual:        true
    }
    tasks: [_t.deployMain]
  }
  pullRequest: {
    environment: "production"
    when: {
      pullRequest: true
    }
    tasks: [_t.deployPreview]
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
    args: ["x", "wrangler", "dev", "--local", "--port", "4321"]
    dependsOn: [_t.build]
  }
  build: {
    command: "bun"
    args: ["run", "build"]
    dependsOn: [_t.generateTypes]
    inputs: [
      "package.json",
      "bun.lock",
      "astro.config.mjs",
      "svelte.config.js",
      "tsconfig.json",
      "wrangler.jsonc",
      "src/**",
      "public/**",
    ]
    outputs: [
      "dist/**",
    ]
  }
  deployPreview: {
    command: "bun"
    args: ["x", "wrangler", "versions", "upload"]
    dependsOn: [_t.build]
    outputs: [
      ".wrangler/**",
    ]
  }
  deployProduction: {
    command: "bun"
    args: ["x", "wrangler", "deploy"]
    dependsOn: [_t.build]
    inputs: [
      "wrangler.jsonc",
      "dist/**",
    ]
    outputs: [
      ".wrangler/**",
    ]
  }
  deployMain: schema.#Task & {
    command: "true"
    dependsOn: [_t.deployProduction]
  }
}
