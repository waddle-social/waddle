package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "waddle-colony"

let _t = tasks

runtime: schema.#ToolsRuntime & {
  flakes: {
    nixpkgs: "github:NixOS/nixpkgs/nixos-25.05"
  }
  tools: {
    node: {
      version: "22.12.0"
      source: schema.#Nix & {
        flake:   "nixpkgs"
        package: "nodejs_22"
      }
    }
  }
}

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
    tasks: [_t.deployProduction]
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
  build: {
    command: "bun"
    args: ["run", "build"]
    dependsOn: [_t.install]
    inputs: [
      "package.json",
      "bun.lock",
      "astro.config.ts",
      "tsconfig.json",
      "wrangler.jsonc",
      "drizzle.config.ts",
      "src/**",
      "public/**",
      "drizzle/**",
    ]
    outputs: [
      "dist/**",
    ]
  }
  dev: {
    command: "bun"
    args: ["run", "dev"]
    dependsOn: [_t.install]
  }
  migrateProduction: {
    command: "bun"
    args: ["x", "wrangler", "d1", "migrations", "apply", "colony", "--remote"]
    dependsOn: [_t.build]
    inputs: [
      "wrangler.jsonc",
      "drizzle/**",
    ]
    outputs: [
      ".wrangler/**",
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
    dependsOn: [_t.migrateProduction]
    inputs: [
      "wrangler.jsonc",
      "dist/**",
      "drizzle/**",
    ]
    outputs: [
      ".wrangler/**",
    ]
  }
}
