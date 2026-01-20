# RFC-0007: AI Features

## Summary

Waddle Social integrates AI capabilities for message summarization, content moderation, translation, and bot/assistant frameworks.

## Motivation

AI can enhance communication by:
- Summarizing long conversations users missed
- Detecting harmful content before human review
- Breaking language barriers with real-time translation
- Enabling intelligent assistants within communities

## Detailed Design

### AI Provider Architecture

Abstract AI provider interface supporting multiple backends:

```rust
trait AIProvider {
    async fn summarize(&self, messages: &[Message]) -> Summary;
    async fn moderate(&self, content: &str) -> ModerationResult;
    async fn translate(&self, text: &str, target: Language) -> String;
    async fn embed(&self, text: &str) -> Vec<f32>;
}
```

Supported providers (configurable per deployment):
- OpenAI (GPT-4, GPT-3.5)
- Anthropic (Claude)
- Local models (Ollama, llama.cpp)
- Custom API endpoints

### Message Summarization

**Catch-up summaries** for users returning to active channels:

```
SummaryRequest
├── channel_id: UUID
├── since: Timestamp (or message_id)
├── max_messages: Integer
└── style: "brief" | "detailed" | "bullet_points"
```

Features:
- Triggered manually by user ("Summarize what I missed")
- Optional scheduled digests (daily/weekly)
- Respects ephemeral message deletion
- Attributes key points to authors

### Content Moderation

**Automated content screening**:

```
ModerationResult
├── flagged: Boolean
├── categories: ModerationCategory[]
├── confidence: Float (0-1)
├── action: "allow" | "flag" | "block"
└── explanation: String
```

Categories:
- `hate_speech`
- `harassment`
- `violence`
- `sexual_content`
- `spam`
- `misinformation`

**Moderation flow**:
1. Message submitted
2. AI moderator evaluates (async, non-blocking)
3. If confidence > threshold, auto-action applied
4. Low-confidence flags queued for human review
5. Human moderator makes final decision

Waddle admins configure:
- Which categories to enforce
- Confidence thresholds per category
- Auto-action vs. human review

### Translation

**Real-time message translation**:

```
TranslationRequest
├── message_id: UUID
├── target_language: LanguageCode
└── source_language: LanguageCode (optional, auto-detect)
```

Features:
- On-demand translation (click to translate)
- Optional auto-translate for user's preferred language
- Original always preserved
- Cached translations for common language pairs

### Semantic Search

AI-powered search using embeddings:

1. Messages embedded on creation
2. Search query embedded
3. Vector similarity search
4. Combined with keyword search for best results

See [RFC-0012: Search](./0012-search.md) for full search design.

### Bot/Assistant Framework

See [RFC-0014: Bot Framework](./0014-bot-framework.md) for detailed design.

AI assistants can:
- Respond to mentions
- Provide contextual help
- Execute commands
- Integrate with external services

## Privacy Considerations

- AI processing is opt-in at Waddle level
- Ephemeral messages excluded from AI processing
- Users can opt-out of AI features personally
- AI provider data handling disclosed
- Self-hosted AI option for sensitive deployments

## Configuration

**Per-Waddle AI settings**:
```json
{
  "ai_features": {
    "summarization": true,
    "moderation": {
      "enabled": true,
      "categories": ["hate_speech", "harassment"],
      "auto_action_threshold": 0.9
    },
    "translation": true,
    "semantic_search": true
  }
}
```

**Per-User AI settings**:
```json
{
  "ai_preferences": {
    "auto_translate_to": "en",
    "allow_ai_processing": true,
    "include_in_summaries": true
  }
}
```

## API Endpoints

```
POST   /channels/:id/summarize        Generate summary
POST   /messages/:id/translate        Translate message
GET    /waddles/:id/moderation/queue  Get flagged content
POST   /moderation/:id/review         Submit human review
```

## Related

- [RFC-0012: Search](./0012-search.md)
- [RFC-0013: Moderation](./0013-moderation.md)
- [RFC-0014: Bot Framework](./0014-bot-framework.md)
- [ADR-0012: Transport Encryption](../adrs/0012-transport-encryption.md) (enables AI processing)
