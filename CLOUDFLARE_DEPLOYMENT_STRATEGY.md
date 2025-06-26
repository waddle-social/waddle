# Cloudflare Workers Deployment Strategy for Waddle

## Overview

This document outlines a comprehensive deployment strategy for the Waddle project using Moonrepo, GitHub Actions, and Cloudflare Workers with SSR (Server-Side Rendering) capabilities.

## Current Architecture

- **Monorepo Tool**: Moonrepo
- **Package Manager**: Bun (v1.2.17)
- **Node Version**: 22.17.0
- **Website Framework**: Astro
- **Project Structure**: `services/website`
- **Deployment Target**: Cloudflare Workers (Edge Computing)

## Why Cloudflare Workers?

- **Edge Computing**: Code runs at 300+ locations globally
- **SSR Support**: Full server-side rendering capabilities
- **Serverless**: No infrastructure to manage
- **Integration**: Access to Cloudflare's full platform (KV, D1, AI, etc.)
- **Performance**: Sub-millisecond cold starts

## Setup Instructions

### Step 1: Install Cloudflare Adapter

Navigate to your website directory and install the adapter:

```bash
cd services/website
bun add @astrojs/cloudflare
```

### Step 2: Configure Astro

Update `services/website/astro.config.ts`:

```typescript
import { defineConfig } from 'astro/config';
import cloudflare from '@astrojs/cloudflare';

export default defineConfig({
  output: 'server',
  adapter: cloudflare({
    mode: 'directory',
    functionPerRoute: false,
    routes: {
      exclude: ['/static/*'],
    },
  }),
});
```

### Step 3: Create Wrangler Configuration

Create `services/website/wrangler.toml`:

```toml
name = "waddle-website"
main = "dist/_worker.js"
compatibility_date = "2025-06-26"

[site]
bucket = "./dist"

[build]
command = "bun run build"

# Environment variables
[vars]
PUBLIC_SITE_URL = "https://waddle.social"
```

### Step 4: Prerequisites

1. **Create Cloudflare API Token**:
   - Log in to Cloudflare Dashboard
   - Navigate to: Account > API Tokens > Create Token
   - Select "Edit Cloudflare Workers" template
   - Configure permissions:
     - Account: Cloudflare Workers Scripts - Edit
     - Account: Account Settings - Read
   - Save the token securely

2. **Get Cloudflare Account ID**:
   - In Cloudflare Dashboard > Workers & Pages
   - Find Account ID in the right sidebar

3. **Configure GitHub Secrets**:
   - Go to your GitHub repository > Settings > Secrets > Actions
   - Add secrets:
     - `CLOUDFLARE_API_TOKEN`: Your API token
     - `CLOUDFLARE_ACCOUNT_ID`: Your account ID

## GitHub Actions Workflow

Create `.github/workflows/deploy.yml`:

```yaml
name: Deploy to Cloudflare Workers
on:
  push:
    branches:
      - main
  pull_request:
    types: [opened, synchronize]

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@v4
        with:
          fetch-depth: 0  # Required for moon to determine affected targets

      - name: Setup Moon toolchain
        uses: moonrepo/setup-toolchain@v0

      - name: Run CI checks
        run: moon ci

      - name: Build website
        run: moon run website:build

      - name: Deploy to Cloudflare Workers
        uses: cloudflare/wrangler-action@v3
        with:
          apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
          workingDirectory: services/website
          command: deploy
          packageManager: bun
```

## Moonrepo Configuration Updates

### 1. Update `services/website/moon.yml`:

```yaml
id: website

language: typescript

fileGroups:
  sources:
    - "src/**/*"

tasks:
  build:
    command: "bun run build"
    options:
      runInCI: true
      outputStyle: stream

  preview:
    command: "bunx wrangler dev"
    deps:
      - "~:build"

  deploy:
    command: "bunx wrangler deploy"
    deps:
      - "~:build"
    options:
      runInCI: false  # Handled by GitHub Actions

  dev:
    command: "bun run dev"
    options:
      persistent: true
      runInCI: false
```

### 2. Update `services/website/package.json` scripts:

```json
{
  "scripts": {
    "dev": "astro dev",
    "build": "astro build",
    "preview": "wrangler dev",
    "deploy": "wrangler deploy",
    "astro": "astro"
  }
}
```

## Local Development

### Setup Wrangler

```bash
cd services/website
bun add -D wrangler
```

### Local Testing with Workers

```bash
# Build and preview locally
moon run website:build
moon run website:preview
```

## Environment Variables

For production builds, add these to your GitHub Actions secrets:

- `CLOUDFLARE_API_TOKEN`: Your Cloudflare API token
- `CLOUDFLARE_ACCOUNT_ID`: Your account ID
- Any Astro public environment variables (PUBLIC_*)

## Preview Deployments

For pull requests, the workflow can create preview deployments:

```yaml
- name: Deploy Preview
  if: github.event_name == 'pull_request'
  uses: cloudflare/wrangler-action@v3
  with:
    apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}
    accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
    workingDirectory: services/website
    command: deploy --env preview
```

## Advanced Features

### 1. Edge Functions

Create API routes in `src/pages/api/`:

```typescript
// src/pages/api/hello.ts
export async function GET({ request, locals }) {
  const runtime = locals.runtime;
  return new Response(JSON.stringify({ 
    message: 'Hello from the edge!',
    cf: request.cf 
  }));
}
```


## Performance Optimizations

1. **Static Asset Caching**:
   - Configure cache headers in `wrangler.toml`
   - Use Cloudflare's CDN for static assets

2. **Code Splitting**:
   - Astro automatically code-splits by route
   - Lazy load heavy components

3. **Edge Caching**:
   - Implement cache headers for API responses
   - Use Cloudflare's Cache API for dynamic content

## Monitoring and Analytics

1. **Workers Analytics**:
   - Monitor performance in Cloudflare Dashboard
   - Track requests, errors, and latency

2. **Custom Analytics**:
   - Integrate with Workers Analytics Engine
   - Send custom metrics

## Rollback Strategy

1. **Version Management**:
   - Each deployment creates a new version
   - Rollback via Cloudflare Dashboard or CLI

2. **Emergency Rollback**:
   ```bash
   bunx wrangler rollback --message "Emergency rollback"
   ```

## Next Steps

1. Install Cloudflare adapter in your project
2. Create wrangler.toml configuration
3. Set up Cloudflare account and create Worker
4. Configure GitHub secrets
5. Create GitHub Actions workflow
6. Test local development with wrangler
7. Deploy to production

## Individual Project Deployments

### Strategy 1: Per-Project Workflows

Create separate workflow files for each project that needs deployment:

#### `.github/workflows/deploy-website.yml`:

```yaml
name: Deploy Website to Cloudflare Workers
on:
  push:
    branches:
      - main
    paths:
      - 'services/website/**'
      - '.moon/workspace.yml'
      - '.moon/toolchain.yml'
  workflow_dispatch:

jobs:
  deploy-website:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Setup Moon toolchain
        uses: moonrepo/setup-toolchain@v0

      - name: Run CI checks for website
        run: moon ci website:build

      - name: Build website
        run: moon run website:build

      - name: Deploy website to Cloudflare Workers
        uses: cloudflare/wrangler-action@v3
        with:
          apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
          workingDirectory: services/website
          command: deploy
          packageManager: bun
```

### Strategy 2: Matrix Build for Multiple Projects

For multiple deployable projects, use a matrix strategy:

#### `.github/workflows/deploy-matrix.yml`:

```yaml
name: Deploy Services to Cloudflare Workers
on:
  push:
    branches:
      - main
  workflow_dispatch:
    inputs:
      project:
        description: 'Project to deploy (leave empty for all)'
        required: false
        type: choice
        options:
          - ''
          - 'website'
          - 'api'
          - 'admin'

jobs:
  detect-changes:
    runs-on: ubuntu-latest
    outputs:
      projects: ${{ steps.detect.outputs.projects }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: moonrepo/setup-toolchain@v0

      - id: detect
        run: |
          if [ "${{ github.event.inputs.project }}" != "" ]; then
            echo "projects=[\"${{ github.event.inputs.project }}\"]" >> $GITHUB_OUTPUT
          else
            # Use moon to detect affected projects
            AFFECTED=$(moon query projects --affected --json | jq -r '[.[] | .id]' | jq -s -c '.')
            echo "projects=$AFFECTED" >> $GITHUB_OUTPUT
          fi

  deploy:
    needs: detect-changes
    if: needs.detect-changes.outputs.projects != '[]'
    runs-on: ubuntu-latest
    strategy:
      matrix:
        project: ${{ fromJson(needs.detect-changes.outputs.projects) }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: moonrepo/setup-toolchain@v0

      - name: Build ${{ matrix.project }}
        run: moon run ${{ matrix.project }}:build

      - name: Deploy ${{ matrix.project }}
        uses: cloudflare/wrangler-action@v3
        with:
          apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
          workingDirectory: services/${{ matrix.project }}
          command: deploy
          packageManager: bun
```

### Strategy 3: Manual Deployment Script

Create a deployment script that can be run locally or in CI:

#### `scripts/deploy.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

PROJECT=${1:-}
ENVIRONMENT=${2:-production}

if [ -z "$PROJECT" ]; then
  echo "Usage: ./scripts/deploy.sh <project> [environment]"
  echo "Projects: website, api, admin"
  exit 1
fi

echo "Deploying $PROJECT to $ENVIRONMENT..."

# Build the project
moon run $PROJECT:build

# Deploy based on project
case $PROJECT in
  website|api|admin)
    cd services/$PROJECT
    bunx wrangler deploy --env $ENVIRONMENT
    ;;
  *)
    echo "Unknown project: $PROJECT"
    exit 1
    ;;
esac
```

Make it executable:
```bash
chmod +x scripts/deploy.sh
```

### Per-Project Configuration

Each project should have its own `wrangler.toml`:

#### `services/api/wrangler.toml`:

```toml
name = "waddle-api"
main = "dist/_worker.js"
compatibility_date = "2025-06-26"

[build]
command = "bun run build"

[env.production]
vars = { ENVIRONMENT = "production" }

[env.preview]
vars = { ENVIRONMENT = "preview" }
```

### Moonrepo Task Configuration

Update `.moon/tasks/deploy.yml` for shared deployment tasks:

```yaml
tasks:
  deploy:
    command: 'bunx wrangler deploy'
    options:
      runInCI: false
    inputs:
      - '@globs(sources)'
      - 'wrangler.toml'
      - 'dist/**/*'

  deploy:preview:
    extends: 'deploy'
    command: 'bunx wrangler deploy --env preview'

  deploy:production:
    extends: 'deploy'
    command: 'bunx wrangler deploy --env production'
```

### Local Development Commands

For individual project management:

```bash
# Deploy specific project
moon run website:deploy

# Deploy to preview environment
moon run api:deploy:preview

# Build and deploy in one command
moon run website:build website:deploy

# Deploy all affected projects
moon run :deploy --affected

# Deploy all projects
moon run :deploy
```

### Environment-Specific Deployments

Configure different environments in `wrangler.toml`:

```toml
# Base configuration
name = "waddle-service"
compatibility_date = "2025-06-26"

# Production environment
[env.production]
name = "waddle-service-prod"
routes = [
  { pattern = "example.com/*", zone_name = "example.com" }
]

# Staging environment
[env.staging]
name = "waddle-service-staging"
routes = [
  { pattern = "staging.example.com/*", zone_name = "example.com" }
]

# Preview environment (for PRs)
[env.preview]
name = "waddle-service-preview-$GITHUB_HEAD_REF"
```

### Deployment Checklist

For each deployable project:

1. ✅ Create `wrangler.toml` in project directory
2. ✅ Update `moon.yml` with deployment tasks
3. ✅ Configure environment variables in GitHub secrets
4. ✅ Create GitHub Actions workflow
5. ✅ Test local deployment with `bunx wrangler dev`
6. ✅ Verify production deployment

## Troubleshooting

### Common Issues:

1. **Build Failures**:
   - Ensure `@astrojs/cloudflare` is installed
   - Check `astro.config.ts` adapter configuration
   - Verify `wrangler.toml` syntax

2. **Runtime Errors**:
   - Check Workers logs in Cloudflare Dashboard
   - Ensure Node.js compatibility flags if needed
   - Verify environment variable bindings

3. **Performance Issues**:
   - Review bundle size with `bunx wrangler deploy --dry-run`
   - Optimize images and assets
   - Implement proper caching strategies