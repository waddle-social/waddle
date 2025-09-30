# RFC-002: Web Integration System

**Status:** Proposed

**Author:** System

**Created:** 2025-09-30

## Abstract

This RFC proposes a web integration system that brings external content (RSS feeds, YouTube videos, GitHub activity) directly into Waddle conversations. The system uses scheduled Cloudflare Workers to poll external sources and publish messages to Waddles.

## Motivation

Modern communities discuss content from across the web:
- **RSS feeds**: Blogs, podcasts, news sites
- **YouTube**: New videos, livestreams
- **GitHub**: Pull requests, releases, issues

Currently, users must manually share links. This RFC proposes automatic integration that:
- Posts new content to Waddles as it's published
- Enriches links with metadata (titles, thumbnails, descriptions)
- Tags content for easy filtering
- Enables conversations about external content within Waddle

## Design

### Architecture

```
┌─────────────────────────────────────────┐
│  External Sources                        │
│  - RSS Feeds                             │
│  - YouTube API                           │
│  - GitHub Webhooks/API                   │
└──────────────┬──────────────────────────┘
               │ Poll (cron) or webhook
               ▼
┌─────────────────────────────────────────┐
│  Integration Workers (Scheduled)         │
│  - RSS Worker (every 15 min)            │
│  - YouTube Worker (every 30 min)        │
│  - GitHub Worker (webhook + fallback)   │
└──────────────┬──────────────────────────┘
               │ Publish events
               ▼
┌─────────────────────────────────────────┐
│  Event Bus                               │
└──────────────┬──────────────────────────┘
               │ Subscribe
               ▼
┌─────────────────────────────────────────┐
│  Chat Worker                             │
│  - Create messages in Waddles            │
│  - Add integration tags                  │
│  - Notify subscribers                    │
└─────────────────────────────────────────┘
```

### Data Model

```typescript
interface Integration {
  id: string;
  waddleId: string;
  type: 'rss' | 'youtube' | 'github';
  name: string;
  config: IntegrationConfig;
  enabled: boolean;
  createdBy: string;
  createdAt: Date;
  lastSync: Date;
  lastError?: string;
}

type IntegrationConfig =
  | RSSConfig
  | YouTubeConfig
  | GitHubConfig;

interface RSSConfig {
  feedUrl: string;
  tags?: string[];              // Custom tags to apply
  postAs?: string;              // Optional bot user ID
  includeContent?: boolean;     // Include full article content
}

interface YouTubeConfig {
  channelId: string;
  playlistId?: string;
  tags?: string[];
  notifyOn: ('video' | 'livestream' | 'short')[];
}

interface GitHubConfig {
  repository: string;           // "owner/repo"
  events: GitHubEvent[];
  tags?: string[];
}

type GitHubEvent =
  | 'pull_request'
  | 'release'
  | 'issue'
  | 'commit'
  | 'discussion';

interface IntegrationItem {
  id: string;
  integrationId: string;
  externalId: string;           // Unique ID from source
  type: string;
  title: string;
  url: string;
  description?: string;
  imageUrl?: string;
  publishedAt: Date;
  metadata: Record<string, any>;
  postedMessageId?: string;     // Waddle message created for this
  postedAt?: Date;
}
```

### Database Schema

```sql
-- Integrations table (per-Waddle D1)
CREATE TABLE integrations (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  name TEXT NOT NULL,
  config JSON NOT NULL,
  enabled BOOLEAN DEFAULT true,
  created_by TEXT NOT NULL,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  last_sync DATETIME,
  last_error TEXT,

  INDEX idx_type (type),
  INDEX idx_enabled (enabled)
);

-- Items fetched from integrations
CREATE TABLE integration_items (
  id TEXT PRIMARY KEY,
  integration_id TEXT NOT NULL,
  external_id TEXT NOT NULL,
  type TEXT NOT NULL,
  title TEXT NOT NULL,
  url TEXT NOT NULL,
  description TEXT,
  image_url TEXT,
  published_at DATETIME NOT NULL,
  metadata JSON,
  posted_message_id TEXT,
  posted_at DATETIME,

  UNIQUE (integration_id, external_id),
  INDEX idx_integration (integration_id, published_at DESC),
  INDEX idx_posted (posted_message_id)
);

-- Central integration tracking (for all Waddles)
CREATE TABLE integration_sync_state (
  integration_id TEXT PRIMARY KEY,
  waddle_id TEXT NOT NULL,
  last_sync_at DATETIME NOT NULL,
  next_sync_at DATETIME NOT NULL,
  sync_count INTEGER DEFAULT 0,
  error_count INTEGER DEFAULT 0,

  INDEX idx_next_sync (next_sync_at),
  INDEX idx_waddle (waddle_id)
);
```

## Implementation

### RSS Integration

```typescript
// integration-worker/rss.ts
import RSSParser from 'rss-parser';

export async function syncRSSIntegrations(env: Env) {
  // Get all enabled RSS integrations that need syncing
  const integrations = await env.CENTRAL_DB.prepare(`
    SELECT i.id, i.waddle_id, i.config
    FROM integration_sync_state s
    JOIN integrations i ON i.id = s.integration_id
    WHERE i.type = 'rss'
      AND i.enabled = true
      AND s.next_sync_at <= datetime('now')
    LIMIT 50
  `).all();

  for (const integration of integrations.results) {
    try {
      await syncRSSFeed(integration, env);

      // Update sync state
      await env.CENTRAL_DB.prepare(`
        UPDATE integration_sync_state
        SET last_sync_at = datetime('now'),
            next_sync_at = datetime('now', '+15 minutes'),
            sync_count = sync_count + 1,
            error_count = 0
        WHERE integration_id = ?
      `).bind(integration.id).run();
    } catch (error) {
      console.error(`RSS sync failed for ${integration.id}:`, error);

      await env.CENTRAL_DB.prepare(`
        UPDATE integration_sync_state
        SET error_count = error_count + 1,
            next_sync_at = datetime('now', '+1 hour')
        WHERE integration_id = ?
      `).bind(integration.id).run();
    }
  }
}

async function syncRSSFeed(integration: any, env: Env) {
  const config: RSSConfig = JSON.parse(integration.config);
  const waddleDb = await getWaddleDb(integration.waddle_id, env);

  // Fetch RSS feed
  const parser = new RSSParser();
  const feed = await parser.parseURL(config.feedUrl);

  // Process new items (reverse to post oldest first)
  for (const item of feed.items.reverse()) {
    const externalId = item.guid || item.link;

    // Check if already posted
    const existing = await waddleDb.prepare(`
      SELECT id FROM integration_items WHERE external_id = ?
    `).bind(externalId).first();

    if (existing) continue;

    // Store item
    const itemId = crypto.randomUUID();
    await waddleDb.prepare(`
      INSERT INTO integration_items (
        id, integration_id, external_id, type, title, url,
        description, image_url, published_at, metadata
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).bind(
      itemId,
      integration.id,
      externalId,
      'rss_item',
      item.title,
      item.link,
      item.contentSnippet,
      item.enclosure?.url,
      new Date(item.pubDate).toISOString(),
      JSON.stringify({ author: item.creator })
    ).run();

    // Create message in Waddle
    const message = formatRSSMessage(item, config);
    const messageId = await createIntegrationMessage(
      integration.waddle_id,
      message,
      ['rss', ...(config.tags || [])],
      env
    );

    // Link message to item
    await waddleDb.prepare(`
      UPDATE integration_items
      SET posted_message_id = ?, posted_at = datetime('now')
      WHERE id = ?
    `).bind(messageId, itemId).run();

    // Publish event
    await publishEvent(env.EVENT_BUS, {
      id: crypto.randomUUID(),
      type: 'integration.rss_item',
      waddleId: integration.waddle_id,
      timestamp: new Date().toISOString(),
      data: {
        feedUrl: config.feedUrl,
        title: item.title,
        link: item.link,
        publishedAt: item.pubDate,
      },
    });
  }
}

function formatRSSMessage(item: any, config: RSSConfig): string {
  let message = `**${item.title}**\n\n`;

  if (config.includeContent && item.contentSnippet) {
    message += `${item.contentSnippet.slice(0, 300)}...\n\n`;
  }

  message += `🔗 ${item.link}`;

  if (item.creator) {
    message += `\n✍️ by ${item.creator}`;
  }

  return message;
}
```

### YouTube Integration

```typescript
// integration-worker/youtube.ts
export async function syncYouTubeIntegrations(env: Env) {
  const integrations = await getYouTubeIntegrations(env);

  for (const integration of integrations) {
    try {
      await syncYouTubeChannel(integration, env);
    } catch (error) {
      console.error(`YouTube sync failed:`, error);
    }
  }
}

async function syncYouTubeChannel(integration: any, env: Env) {
  const config: YouTubeConfig = JSON.parse(integration.config);

  // Fetch recent uploads from YouTube API
  const response = await fetch(
    `https://www.googleapis.com/youtube/v3/playlistItems?` +
    new URLSearchParams({
      part: 'snippet',
      playlistId: config.playlistId || `UU${config.channelId.slice(2)}`, // Uploads playlist
      maxResults: '10',
      key: env.YOUTUBE_API_KEY,
    })
  );

  const data = await response.json();

  for (const item of data.items) {
    const videoId = item.snippet.resourceId.videoId;
    const externalId = `youtube:${videoId}`;

    // Check if already posted
    const waddleDb = await getWaddleDb(integration.waddle_id, env);
    const existing = await waddleDb.prepare(`
      SELECT id FROM integration_items WHERE external_id = ?
    `).bind(externalId).first();

    if (existing) continue;

    // Store and post
    const message = formatYouTubeMessage(item);
    const messageId = await createIntegrationMessage(
      integration.waddle_id,
      message,
      ['youtube', ...(config.tags || [])],
      env
    );

    await waddleDb.prepare(`
      INSERT INTO integration_items (
        id, integration_id, external_id, type, title, url,
        image_url, published_at, posted_message_id, posted_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
    `).bind(
      crypto.randomUUID(),
      integration.id,
      externalId,
      'youtube_video',
      item.snippet.title,
      `https://www.youtube.com/watch?v=${videoId}`,
      item.snippet.thumbnails.high.url,
      new Date(item.snippet.publishedAt).toISOString(),
      messageId
    ).run();
  }
}

function formatYouTubeMessage(item: any): string {
  const videoId = item.snippet.resourceId.videoId;
  return `🎥 **New video: ${item.snippet.title}**\n\n` +
         `${item.snippet.description.slice(0, 200)}...\n\n` +
         `https://www.youtube.com/watch?v=${videoId}`;
}
```

### GitHub Integration

```typescript
// integration-worker/github.ts
export async function handleGitHubWebhook(request: Request, env: Env) {
  // Verify webhook signature
  const signature = request.headers.get('X-Hub-Signature-256');
  if (!await verifyWebhookSignature(request, signature, env.GITHUB_WEBHOOK_SECRET)) {
    return new Response('Invalid signature', { status: 401 });
  }

  const event = request.headers.get('X-GitHub-Event');
  const payload = await request.json();

  // Find integrations for this repository
  const repo = payload.repository.full_name;
  const integrations = await env.CENTRAL_DB.prepare(`
    SELECT i.id, i.waddle_id, i.config
    FROM integrations i
    WHERE i.type = 'github'
      AND i.enabled = true
      AND JSON_EXTRACT(i.config, '$.repository') = ?
  `).bind(repo).all();

  for (const integration of integrations.results) {
    const config: GitHubConfig = JSON.parse(integration.config);

    // Check if this event is enabled
    if (!config.events.includes(event as GitHubEvent)) {
      continue;
    }

    // Process event
    let message: string;
    switch (event) {
      case 'pull_request':
        if (payload.action === 'opened') {
          message = formatGitHubPR(payload.pull_request);
          await postGitHubMessage(integration.waddle_id, message, config.tags, env);
        }
        break;

      case 'release':
        if (payload.action === 'published') {
          message = formatGitHubRelease(payload.release);
          await postGitHubMessage(integration.waddle_id, message, config.tags, env);
        }
        break;

      // Handle other events...
    }
  }

  return new Response('OK');
}

function formatGitHubPR(pr: any): string {
  return `🔀 **Pull Request #${pr.number}: ${pr.title}**\n\n` +
         `${pr.body?.slice(0, 200) || 'No description'}...\n\n` +
         `👤 by @${pr.user.login}\n` +
         `🔗 ${pr.html_url}`;
}

function formatGitHubRelease(release: any): string {
  return `🚀 **Release ${release.tag_name}: ${release.name}**\n\n` +
         `${release.body?.slice(0, 300) || ''}...\n\n` +
         `🔗 ${release.html_url}`;
}
```

### Integration Message Creation

```typescript
async function createIntegrationMessage(
  waddleId: string,
  content: string,
  tags: string[],
  env: Env
): Promise<string> {
  const waddleDb = await getWaddleDb(waddleId, env);

  // Use system/bot user
  const botUserId = 'system-bot';
  const messageId = crypto.randomUUID();

  // Create message
  await waddleDb.prepare(`
    INSERT INTO messages (id, user_id, content, raw_content, created_at)
    VALUES (?, ?, ?, ?, ?)
  `).bind(
    messageId,
    botUserId,
    content,
    content,
    new Date().toISOString()
  ).run();

  // Add tags
  for (const tag of tags) {
    await waddleDb.prepare(`
      INSERT INTO message_tags (message_id, tag, source)
      VALUES (?, ?, 'human')
    `).bind(messageId, tag).run();
  }

  // Broadcast via Durable Object
  const doId = env.WADDLE_DO.idFromName(waddleId);
  const stub = env.WADDLE_DO.get(doId);
  await stub.fetch(new Request('https://do/broadcast', {
    method: 'POST',
    body: JSON.stringify({
      type: 'message',
      messageId,
      userId: botUserId,
      content,
    }),
  }));

  return messageId;
}
```

### Scheduled Workers (wrangler.toml)

```toml
# RSS Worker - runs every 15 minutes
[[workflows]]
name = "rss-sync"
schedule = "*/15 * * * *"
script_name = "integration-worker"
entrypoint = "syncRSSIntegrations"

# YouTube Worker - runs every 30 minutes
[[workflows]]
name = "youtube-sync"
schedule = "*/30 * * * *"
script_name = "integration-worker"
entrypoint = "syncYouTubeIntegrations"

# GitHub Worker - webhook + hourly fallback
[[workflows]]
name = "github-sync"
schedule = "0 * * * *"
script_name = "integration-worker"
entrypoint = "syncGitHubIntegrations"
```

## Management UI

Users configure integrations via GraphQL:

```graphql
mutation CreateIntegration {
  createIntegration(input: {
    waddleId: "waddle-123"
    type: RSS
    name: "Rawkode Academy Blog"
    config: {
      feedUrl: "https://rawkode.academy/feed.xml"
      tags: ["blog", "tutorials"]
      includeContent: false
    }
  }) {
    id
    name
    enabled
  }
}

query ListIntegrations {
  waddle(id: "waddle-123") {
    integrations {
      id
      type
      name
      enabled
      lastSync
      lastError
    }
  }
}
```

## Security Considerations

- **Rate limiting**: Respect API rate limits from external services
- **Authentication**: Store API keys securely in Cloudflare Secrets
- **Webhook verification**: Always verify webhook signatures
- **Content filtering**: Sanitize external content before posting
- **Spam prevention**: Limit integration message frequency

## Future Enhancements

- **Slack/Discord imports**: Import historical messages
- **Twitter/X integration**: Post tweets to Waddle
- **Calendar integrations**: Google Calendar, Outlook
- **CI/CD status**: CircleCI, GitHub Actions results
- **Monitoring alerts**: Datadog, Sentry alerts

## References

- [RSS 2.0 Specification](https://www.rssboard.org/rss-specification)
- [YouTube Data API](https://developers.google.com/youtube/v3)
- [GitHub Webhooks](https://docs.github.com/en/developers/webhooks-and-events/webhooks/webhook-events-and-payloads)