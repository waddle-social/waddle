# Infrastructure Setup

## Purpose

Configure and manage all Cloudflare infrastructure resources required for Huddle, including databases, storage, queues, and security policies.

## Resources Overview

### D1 Databases
- `huddle-db` - Production database
- `huddle-db-dev` - Development database

### KV Namespaces
- `huddle-features` - Feature flags and configuration
- `huddle-graph` - Social graph cache
- `huddle-oauth` - OAuth state management
- `huddle-dedupe` - Webhook deduplication

### R2 Buckets
- `huddle-files` - ICS files, exports, audit logs

### Queues
- `calendar-tasks` - Calendar sync operations
- `notify-tasks` - Notification delivery
- `index-tasks` - Firehose indexing
- `calendar-dlq` - Dead letter queue for calendar
- `notify-dlq` - Dead letter queue for notifications

### Analytics Engine Datasets
- `product_metrics` - Product KPIs
- `sre_kpis` - Operational metrics

### Cloudflare Access
- Application for `colony.huddle.waddle.social`
- Service tokens for inter-worker communication

### Turnstile
- Site configuration for `huddle.waddle.social`

## Setup Scripts

### scripts/setup-d1.sh
```bash
#!/bin/bash

# Create production database
wrangler d1 create huddle-db

# Create development database
wrangler d1 create huddle-db-dev

# Get database IDs and update wrangler.toml files
echo "Update wrangler.toml files with database IDs"
```

### scripts/setup-kv.sh
```bash
#!/bin/bash

# Create KV namespaces
wrangler kv:namespace create huddle-features
wrangler kv:namespace create huddle-graph
wrangler kv:namespace create huddle-oauth
wrangler kv:namespace create huddle-dedupe

# Preview namespaces for development
wrangler kv:namespace create huddle-features --preview
wrangler kv:namespace create huddle-graph --preview
wrangler kv:namespace create huddle-oauth --preview
wrangler kv:namespace create huddle-dedupe --preview
```

### scripts/setup-r2.sh
```bash
#!/bin/bash

# Create R2 bucket
wrangler r2 bucket create huddle-files

# Set up lifecycle rules
wrangler r2 bucket lifecycle set huddle-files --config lifecycle.json

# Set up CORS
wrangler r2 bucket cors set huddle-files --config cors.json
```

### scripts/setup-queues.sh
```bash
#!/bin/bash

# Create queues
wrangler queues create calendar-tasks
wrangler queues create notify-tasks
wrangler queues create index-tasks

# Create DLQs
wrangler queues create calendar-dlq
wrangler queues create notify-dlq

# Configure DLQ settings
wrangler queues update calendar-tasks --dead-letter-queue calendar-dlq
wrangler queues update notify-tasks --dead-letter-queue notify-dlq
```

## Configuration Files

### cloudflare/access-policies.json
```json
{
  "colony_admin": {
    "name": "Colony Admin Access",
    "audience": "colony.huddle.waddle.social",
    "include": [
      {
        "email": {
          "domain": "waddle.social"
        }
      }
    ],
    "require": [
      {
        "devicePosture": ["managed"]
      }
    ],
    "sessionDuration": "8h",
    "enableBindingCookie": true
  }
}
```

### cloudflare/turnstile.json
```json
{
  "siteKey": "...",
  "secretKey": "...",
  "domains": [
    "huddle.waddle.social",
    "localhost:4321"
  ],
  "mode": "managed",
  "challengePassage": "30m",
  "appearance": {
    "theme": "auto",
    "language": "en"
  }
}
```

### cloudflare/r2-lifecycle.json
```json
{
  "rules": [
    {
      "id": "cleanup-old-ics",
      "status": "Enabled",
      "filter": {
        "prefix": "ics/"
      },
      "actions": {
        "expiration": {
          "days": 90
        }
      }
    },
    {
      "id": "archive-audit-logs",
      "status": "Enabled",
      "filter": {
        "prefix": "audit/"
      },
      "actions": {
        "transition": {
          "storageClass": "GLACIER",
          "days": 30
        }
      }
    }
  ]
}
```

### cloudflare/r2-cors.json
```json
{
  "CORSRules": [
    {
      "AllowedOrigins": ["https://huddle.waddle.social"],
      "AllowedMethods": ["GET", "HEAD"],
      "AllowedHeaders": ["*"],
      "MaxAgeSeconds": 3600
    }
  ]
}
```

## Environment Configuration

### Production Variables
```env
# OAuth Providers
GOOGLE_CLIENT_ID=...
GOOGLE_CLIENT_SECRET=...
MICROSOFT_CLIENT_ID=...
MICROSOFT_CLIENT_SECRET=...

# Encryption
OAUTH_ENC_KEY=... # 32-byte key for AES-256-GCM

# Turnstile
TURNSTILE_SITE_KEY=...
TURNSTILE_SECRET=...

# Cloudflare
CF_ACCOUNT_ID=...
CF_API_TOKEN=...

# ATProto
FIREHOSE_URL=wss://bsky.network/xrpc/com.atproto.sync.subscribeRepos
```

### Development Variables
```env
# Local services
APPVIEW_URL=http://localhost:8787
FIREHOSE_URL=http://localhost:8788
CALENDAR_SYNC_URL=http://localhost:8789
WEBHOOKS_URL=http://localhost:8790
NOTIFY_URL=http://localhost:8791

# Miniflare
MINIFLARE_D1_PERSIST=./.mf/d1
MINIFLARE_KV_PERSIST=./.mf/kv
MINIFLARE_R2_PERSIST=./.mf/r2
```

## Security Configuration

### WAF Rules
```json
{
  "rules": [
    {
      "name": "Block suspicious user agents",
      "expression": "(http.user_agent contains \"bot\" and not http.user_agent contains \"googlebot\")",
      "action": "block"
    },
    {
      "name": "Rate limit API",
      "expression": "(http.request.uri.path matches \"^/api/\")",
      "action": "rate_limit",
      "rateLimit": {
        "requests": 100,
        "period": 60
      }
    }
  ]
}
```

### Page Rules
```json
{
  "rules": [
    {
      "targets": ["/api/*"],
      "actions": {
        "cache_level": "bypass",
        "security_level": "high"
      }
    },
    {
      "targets": ["/static/*"],
      "actions": {
        "cache_level": "aggressive",
        "edge_cache_ttl": 86400
      }
    }
  ]
}
```

## Monitoring Setup

### Logpush Configuration
```json
{
  "dataset": "workers_trace_events",
  "destination": "r2://huddle-logs",
  "filter": {
    "where": {
      "key": "ScriptName",
      "operator": "contains",
      "value": "huddle"
    }
  }
}
```

### Alerting Rules
```json
{
  "alerts": [
    {
      "name": "High error rate",
      "condition": "rate(errors) > 0.01",
      "channels": ["email", "slack"]
    },
    {
      "name": "Queue backup",
      "condition": "queue_depth > 10000",
      "channels": ["pagerduty"]
    }
  ]
}
```

## Deployment Pipeline

### GitHub Actions Workflow
```yaml
name: Deploy Infrastructure
on:
  push:
    branches: [main]
    paths:
      - 'infrastructure/**'

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup Cloudflare CLI
        run: npm install -g wrangler
      
      - name: Deploy D1 Migrations
        run: |
          for service in services/*/; do
            if [ -d "$service/migrations" ]; then
              wrangler d1 migrations apply huddle-db --local=false
            fi
          done
        env:
          CLOUDFLARE_API_TOKEN: ${{ secrets.CF_API_TOKEN }}
      
      - name: Update KV Configuration
        run: |
          wrangler kv:key put --namespace-id=$FEATURES_NS "config" "$(cat config.json)"
        env:
          CLOUDFLARE_API_TOKEN: ${{ secrets.CF_API_TOKEN }}
```

## Cost Optimization

### Resource Limits
- D1: 500MB per database
- KV: 1GB total storage
- R2: 10GB included
- Queues: 1M messages/month
- Workers: 100k requests/day

### Optimization Strategies
- Use KV for hot data only
- Implement TTLs on cache entries
- Archive old data to R2
- Use queue batching
- Enable Argo Smart Routing

## Disaster Recovery

### Backup Strategy
- Daily D1 exports to R2
- KV snapshots every 6 hours
- Queue message replay capability
- Configuration version control

### Recovery Procedures
1. Database restoration from R2
2. KV rebuild from D1
3. Queue replay from DLQ
4. Service rollback via wrangler

## Maintenance

### Regular Tasks
- Weekly backup verification
- Monthly cost review
- Quarterly security audit
- Annual disaster recovery drill

### Monitoring Checklist
- [ ] All services healthy
- [ ] Queue depths normal
- [ ] Error rates acceptable
- [ ] Database performance good
- [ ] Cost within budget

## Future Enhancements

- Multi-region deployment
- Database read replicas
- Advanced caching strategies
- Cost anomaly detection
- Automated scaling policies
- Infrastructure as Code (Terraform)