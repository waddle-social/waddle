# Waddle

## Workspace Quickstart

This repository uses a single workspace at the root with internal catalogs.

- Install: `bun install`
- Build: `bun run build`
- Test: `bun test`

See full instructions: `specs/001-migrate-bun-workspaces/quickstart.md`.
A modern web application built with Astro and deployed on Cloudflare Workers.

## Tech Stack

- **Framework**: Astro with SSR
- **Deployment**: Cloudflare Workers
- **Build System**: Moon
- **Package Manager**: Bun
- **Language**: TypeScript

## Getting Started

```bash
# Install dependencies
bun install

# Development
moon run website:dev

# Build
moon run website:build
```

## Deployment

Automatic deployments via GitHub Actions:
- **Pull Requests**: Preview deployments
- **Main branch**: Production deployment to waddle.social and waddle.chat

## Project Structure

```
waddle/
├── services/
│   └── website/        # Main web application
└── .moon/             # Moon build configuration
```
