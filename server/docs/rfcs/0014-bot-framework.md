# RFC-0014: Bot/Assistant Framework

## Summary

The bot framework enables third-party bots and AI assistants to participate in Waddles, providing automation, integrations, and interactive experiences.

## Motivation

Communities benefit from:
- Automation (welcome messages, reminders)
- External integrations (GitHub, Jira, etc.)
- AI assistants for Q&A and help
- Custom commands and workflows
- Games and entertainment

## Detailed Design

### Bot Identity

Bots are special user accounts:

```
Bot
├── id: UUID
├── did: DID (bot's own identity)
├── owner_did: DID (creator)
├── name: String
├── avatar: URL
├── description: String
├── bot_type: "standard" | "ai_assistant" | "webhook"
├── permissions: BotPermissions
├── created_at: Timestamp
└── verified: Boolean (official bots)
```

### Bot Types

**1. Standard Bots**:
- Full API access via OAuth
- Can read messages, send messages, react
- Requires explicit invitation to Waddles
- Runs on developer's infrastructure

**2. AI Assistants**:
- LLM-powered conversational bots
- Can be based on built-in AI or external
- Context-aware (channel history access)
- Rate-limited to prevent abuse

**3. Webhook Bots**:
- Simplified one-way posting
- Receives webhook URL
- Posts messages via HTTP POST
- No read access, only send

### Bot Permissions

```
BotPermissions
├── scopes: Scope[]
├── allowed_waddles: UUID[] | "all"
└── rate_limits: RateLimits

Scope
├── messages.read: Read messages in allowed channels
├── messages.write: Send messages
├── messages.manage: Delete own messages
├── reactions.add: Add reactions
├── presence.read: See online users
├── members.read: List Waddle members
├── channels.read: List channels
└── webhooks.manage: Manage webhooks
```

### Bot Invocation

**Slash Commands**:
```
/remind me in 1 hour to check the build
/poll "Best mascot?" :penguin: :duck: :owl:
/github link waddle-social/wa#123
```

Command registration:
```
SlashCommand
├── name: String (e.g., "remind")
├── description: String
├── options: CommandOption[]
└── bot_id: UUID
```

**Mentions**:
```
@assistant-bot how do I configure webhooks?
```

**Patterns** (advanced):
```
PatternTrigger
├── regex: String
├── channel_ids: UUID[] (optional)
└── response_type: "reply" | "dm"
```

### Bot Events

Bots receive events via WebSocket or webhook:

```
BotEvent
├── type: EventType
├── waddle_id: UUID
├── channel_id: UUID
├── data: EventData
└── timestamp: Timestamp

EventTypes:
├── message.created
├── message.updated
├── message.deleted
├── reaction.added
├── member.joined
├── member.left
├── command.invoked
└── mention.received
```

### AI Assistant Framework

Built-in AI assistant capabilities:

```
AIAssistantConfig
├── bot_id: UUID
├── provider: "openai" | "anthropic" | "custom"
├── model: String
├── system_prompt: String
├── context_window: Integer (messages to include)
├── temperature: Float
└── knowledge_base: KnowledgeBase[]
```

**Knowledge Base**:
- Custom documents for RAG
- Waddle-specific information
- FAQ and help content

### Webhook Integration

Simple webhook posting:

```bash
curl -X POST https://waddle.social/hooks/abc123 \
  -H "Content-Type: application/json" \
  -d '{"content": "Build passed! ✅"}'
```

Webhook payload:
```json
{
  "content": "Message text",
  "embeds": [...],
  "username": "GitHub Bot",
  "avatar_url": "https://..."
}
```

### Bot Marketplace

Discovery and installation:

```
BotListing
├── bot_id: UUID
├── name: String
├── description: String
├── categories: String[]
├── install_count: Integer
├── rating: Float
├── verified: Boolean
└── source_url: URL (if open source)
```

### Security Considerations

- Bots cannot impersonate users
- Rate limits prevent spam
- Sensitive data redacted from bot events
- Webhook secrets for verification
- Waddle admins control bot installation

### Rate Limits

| Bot Type | Messages/min | API calls/min |
|----------|--------------|---------------|
| Standard | 30 | 120 |
| AI Assistant | 10 | 60 |
| Webhook | 60 | N/A |

## API Endpoints

```
# Bot management
POST   /bots                        Create bot
GET    /bots/:id                    Get bot details
PATCH  /bots/:id                    Update bot
DELETE /bots/:id                    Delete bot
POST   /bots/:id/token              Regenerate token

# Bot installation
POST   /waddles/:id/bots            Install bot
DELETE /waddles/:id/bots/:bot_id    Remove bot
GET    /waddles/:id/bots            List installed bots

# Commands
POST   /bots/:id/commands           Register command
DELETE /bots/:id/commands/:name     Remove command

# Webhooks
POST   /channels/:id/webhooks       Create webhook
GET    /channels/:id/webhooks       List webhooks
DELETE /webhooks/:id                Delete webhook
POST   /webhooks/:id                Post via webhook (public)

# Marketplace
GET    /bots/discover               Browse bots
GET    /bots/categories             List categories
```

## Bot SDK

Provide SDK for common languages:

```rust
// Rust example
use waddle_bot_sdk::{Bot, Event};

#[tokio::main]
async fn main() {
    let bot = Bot::new(env::var("BOT_TOKEN")?);

    bot.on_command("ping", |ctx| async {
        ctx.reply("Pong! 🏓").await
    });

    bot.on_message(|ctx, msg| async {
        if msg.mentions_bot() {
            ctx.reply("Hello! How can I help?").await
        }
    });

    bot.start().await
}
```

## Related

- [RFC-0007: AI Features](./0007-ai-integrations.md)
- [RFC-0004: Rich Message Format](./0004-message-format.md)
- [ADR-0005: ATProto Identity](../adrs/0005-atproto-identity.md)
- [Spec: API Contracts](../specs/api-contracts.md)
