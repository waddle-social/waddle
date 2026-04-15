package cuenv

import "github.com/cuenv/cuenv/schema"

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
    annotations: "Preview URL": schema.#TaskCaptureRef & {
      cuenvTask:    "deployPreview"
      cuenvCapture: "previewUrl"
    }
  }
}

tasks: {
  install: {
    command: "bun"
    args: ["install", "--frozen-lockfile", "--cwd", ".."]
    inputs: [
      "../package.json",
      "../bun.lock",
      "package.json",
    ]
  }
  generateTypes: {
    command: "bun"
    args: ["run", "generate-types"]
    dependsOn: [_t.install]
    inputs: [
      "../package.json",
      "../bun.lock",
      "package.json",
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
      "../package.json",
      "../bun.lock",
      "package.json",
      "astro.config.mjs",
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
    captures: previewUrl: {
      pattern: "Version Preview URL: (.+)"
    }
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
