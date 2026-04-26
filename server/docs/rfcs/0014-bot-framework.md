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

## Shared Plugin Framework Implementation Spec

This section is the implementation target for sample plugins. It supersedes any
bot-specific HTTP or webhook sketches in this RFC for the sample plugin work:
sample plugins run in Waddle's shared plugin framework, interact through
XMPP-native messages, PubSub nodes, ad-hoc commands, data forms, and first-party
host grants, and never receive client secrets or user secrets.

The existing Waddle extension runtime already models one plugin capability:
message enrichment. The sample plugin framework must keep that capability, but
must not make enrichment the whole plugin model. A plugin can declare one or
more capabilities, and an installed plugin instance can use only the grants that
the host approved for that Waddle.

### Framework Rules

- Plugins are packaged as Waddle extension OCI artifacts containing a WASM
  component plus a `waddle-plugin.json` manifest.
- The manifest declares capabilities. The Waddle host turns capabilities into
  install-time grant prompts and passes only approved, scoped handles to the
  plugin.
- Capabilities describe what the component exports; grants describe which
  Waddle, room, PubSub node prefix, message stream, AI provider, upload service,
  or public-network egress the installed instance can use.
- Plugins never receive OAuth client secrets, user access tokens, user refresh
  tokens, browser cookies, raw session tokens, or arbitrary server credentials.
  If a provider needs credentials, the Waddle deployment config owns them and
  exposes a narrow host capability such as `ai.invoke.v1`.
- Structured protocol data must be typed at every boundary. Stanzas use typed
  XMPP values, PubSub payloads use typed XML structs, JIDs use typed JID values,
  and plugin events use typed enums. `String` is allowed only for human-facing
  text, labels, prompt text, URLs, and log messages.
- Official XMPP namespaces are used only with conformant wire shapes. Waddle
  plugin-specific payloads use `urn:waddle:plugin:*` namespaces.
- State is persisted through XEP-0060 PubSub nodes unless the spec marks it
  ephemeral. Private per-user state follows the XEP-0223 private-storage profile
  where applicable. Creation and configuration use XEP-0050 ad-hoc commands and
  XEP-0004 data forms.
- Custom rich messages that would be confusing to clients without plugin
  support must include a readable body and XEP-0428 fallback markers. Enrichment
  that leaves the original message body as the primary content does not need a
  fallback body.
- If Waddle advertises `urn:xmpp:fasten:0`, any later enrichment attached to an
  already delivered message must use XEP-0422 exactly. Until then, enrichment is
  applied only while the host is processing the original outbound message.
- Plugins may publish service discovery features for their own custom
  namespaces. If they advertise official features such as XEP-0004, XEP-0050,
  XEP-0060, XEP-0359, XEP-0422, XEP-0428, XEP-0444, or XEP-0363, tests must
  assert the exact XEP-defined shape.

### Manifest Contract

Each sample plugin manifest uses this shape:

```json
{
  "id": "io.waddle.samples.link-library",
  "name": "Link Library",
  "version": "0.1.0",
  "namespace": "urn:waddle:plugin:link-library:0",
  "runtime": {
    "world": "waddle-extension",
    "minimumHostVersion": "0.1.0"
  },
  "capabilities": [],
  "requiredGrants": [],
  "optionalGrants": [],
  "stateNodes": [],
  "uiSurfaces": [],
  "rateLimits": {}
}
```

Capability names for the samples:

| Capability | Meaning |
|------------|---------|
| `message.enrich.v1` | Observe an outbound message body and detected links, then return typed XML payloads before fanout. |
| `message.read-recent.v1` | Read a host-bounded recent message window for a declared room-scoped purpose. |
| `message.respond.v1` | Send host-authored messages as the installed plugin actor into approved rooms. |
| `command.handle.v1` | Expose XEP-0050 command nodes backed by XEP-0004 forms. |
| `pubsub.state.v1` | Read or write approved PubSub node prefixes through typed host APIs. |
| `ui.surface.v1` | Provide declarative UI surface descriptors rendered by first-party Waddle clients. |
| `ai.invoke.v1` | Request AI work from a deployment-owned provider without seeing provider secrets. |
| `http.fetch-public.v1` | Fetch anonymous public HTTP(S) metadata with host allowlists, size limits, and timeouts. |
| `file.upload.v1` | Request first-party XEP-0363 upload slots for plugin-generated assets. |
| `members.read.v1` | Read approved room member display metadata, never private profile secrets. |

Grant names for the samples:

| Grant | Scope |
|-------|-------|
| `messages:enrich:room` | Enrich messages in configured rooms. |
| `messages:read:recent` | Read bounded recent room history for a declared purpose. |
| `messages:write:room` | Send plugin-authored messages to configured rooms. |
| `commands:execute:room` | Handle configured XEP-0050 command nodes in a room. |
| `pubsub:read:prefix` | Read state under one plugin-owned PubSub node prefix. |
| `pubsub:write:prefix` | Publish, retract, or purge state under one plugin-owned PubSub node prefix. |
| `members:read:room` | Read member display names and bare JIDs for configured rooms. |
| `ai:invoke:deployment` | Invoke the deployment-configured AI provider through host policy. |
| `http:fetch:public` | Fetch public HTTP(S) URLs with no cookies, credentials, or private-network access. |
| `upload:request:room` | Request upload slots from Waddle's XEP-0363 service for plugin output. |

### Common State and Events

The host allocates a PubSub node prefix for each installed plugin instance:

```text
waddle/plugins/{plugin_id}/{installation_id}/
```

Plugins refer to state nodes by typed `PluginStateNode` values, not by
constructing raw node strings at call sites. Node IDs in this RFC are templates
for implementers and tests.

Common state records:

```text
PluginInstallation
├── installation_id: PluginInstallationId
├── plugin_id: PluginId
├── waddle_id: WaddleId
├── installed_by: BareJid
├── grants: Vec<PluginGrant>
├── enabled: bool
├── created_at: Timestamp
└── updated_at: Timestamp

PluginEvent
├── event_id: EventId
├── installation_id: PluginInstallationId
├── room: Option<BareJid>
├── actor: PluginActorJid
├── kind: PluginEventKind
├── payload: PluginEventPayload
└── occurred_at: Timestamp
```

Common event kinds:

- `plugin.installed`
- `plugin.configured`
- `plugin.disabled`
- `plugin.error.recorded`
- `message.enrichment.completed`
- `message.enrichment.skipped`
- `state.node.published`
- `state.node.retracted`

Common nodes:

| Node suffix | Access policy | Purpose |
|-------------|---------------|---------|
| `manifest` | members-read, admins-write | Installed manifest snapshot and resolved feature list. |
| `config` | admins-read, admins-write | Non-secret install configuration. |
| `events` | members-read, plugin-write | Auditable plugin lifecycle and domain events. |
| `errors` | admins-read, plugin-write | Redacted operational errors. |

### Common UI Surfaces

All plugin UI is declarative data rendered by Waddle clients. Plugins do not
ship arbitrary client JavaScript, and clients do not pass secrets to plugins.

| Surface | Where it appears | Backing protocol |
|---------|------------------|------------------|
| `admin-settings` | Waddle or room plugin settings | XEP-0050 command plus XEP-0004 form. |
| `composer-command` | Composer command menu | XEP-0050 command metadata. |
| `message-inline` | A message card or attached rich payload | XMPP message payload and optional XEP-0428 fallback. |
| `room-sidebar` | Room-level side panel | PubSub read of plugin state nodes. |
| `detail-modal` | Focused plugin object view | PubSub item retrieval plus command actions. |
| `bot-profile` | Plugin actor profile | Service discovery plus plugin manifest metadata. |

### Common Failure and Fallback Behavior

- Enrichment is fail-open. If a plugin fails, times out, returns invalid XML, or
  lacks a grant, the original message is delivered unchanged and a redacted
  `plugin.error.recorded` event is published for admins.
- Command handling is fail-closed. If a command cannot validate permissions or
  form data, the plugin returns an XEP-0050 completed command containing a
  `note type='error'` and does not mutate state.
- State publishing is idempotent where possible. Retried publishes use stable
  item IDs, and duplicate command submissions must not duplicate visible cards.
- Unsupported clients must see a useful body for plugin-authored messages.
  Supporting clients hide or de-emphasize fallback text using XEP-0428.
- Public HTTP fetch failures produce partial metadata, not a blocked message.
  The host must not follow private-network redirects or send cookies.
- AI failures produce a short plugin-authored failure message or status update,
  never raw provider errors or secrets.
- When a required grant is removed, the host disables only the affected
  capability and leaves readable historical state intact.

## Sample Plugins and Acceptance Tests

The tests below are acceptance requirements, not implementation instructions.
Each server-side test should be implemented as a dedicated Rust custom test
suite for the XEP behavior it exercises. Client rendering tests should live in
the chat test suite when they assert Waddle UI surfaces.

### Link Library

Link Library enriches messages that contain public links and builds a searchable
room link index.

Manifest capabilities:

- `message.enrich.v1`
- `pubsub.state.v1`
- `ui.surface.v1`
- `http.fetch-public.v1`
- `command.handle.v1`

Required grants:

- `messages:enrich:room` for configured rooms.
- `pubsub:read:prefix` and `pubsub:write:prefix` under
  `waddle/plugins/io.waddle.samples.link-library/{installation_id}/`.
- `http:fetch:public` for anonymous metadata fetches.
- `commands:execute:room` for `waddle:link-library:configure` and
  `waddle:link-library:list`.

Optional grants:

- None. Link Library must not require external API credentials.

Data model:

```text
LinkRecord
├── link_id: LinkId
├── canonical_url: Url
├── original_url: Url
├── room_jid: BareJid
├── source_message_id: StableMessageId
├── source_sender: BareJid
├── posted_at: Timestamp
├── title: Option<HumanText>
├── description: Option<HumanText>
├── image_url: Option<Url>
├── site_name: Option<HumanText>
├── content_type: Option<MimeType>
├── fetch_status: LinkFetchStatus
└── tags: Vec<LinkTag>

LinkFetchStatus = pending | fetched | unsupported | failed | blocked
```

State nodes and events:

| Node suffix | Item ID | Payload namespace | Event |
|-------------|---------|-------------------|-------|
| `links/by-message/{message_id}` | `{link_id}` | `urn:waddle:plugin:link-library:0` | `link.detected` |
| `links/by-url/{url_hash}` | `{link_id}` | `urn:waddle:plugin:link-library:0` | `link.metadata.updated` |
| `links/index` | `{posted_at}:{link_id}` | `urn:waddle:plugin:link-library:0` | `link.indexed` |
| `config` | `default` | `urn:waddle:plugin:link-library:0` | `link.config.updated` |

The message enrichment payload uses
`<link-preview xmlns='urn:waddle:plugin:link-library:0'>`. It must be built as
a typed XML element and appended to the original message before fanout. The
original body remains the fallback for clients without Link Library support.

UI surfaces:

- `message-inline`: link preview block in the source message card.
- `room-sidebar`: filterable link library with title, site, sender, and date.
- `composer-command`: `/links` command to open the sidebar or return recent
  links as a data-form result.
- `admin-settings`: allowlist or blocklist hosts, max links per message, and
  preview image toggle.

Failure and fallback behavior:

- If metadata fetch times out, publish a `LinkRecord` with
  `fetch_status=pending` or `failed` and render the original URL only.
- If the URL resolves to private IP space, publish `fetch_status=blocked` and
  do not fetch bytes.
- If enrichment returns malformed XML, drop only the preview payload and deliver
  the original message.
- If PubSub write fails after the message fanout, do not resend the message.
  Publish a redacted admin error when the state node becomes available.

Acceptance tests:

1. `link_library_manifest_declares_only_sample_grants`: loading the manifest
   returns the five capabilities above, no `messages:read:recent` grant, and no
   secret or token fields.
2. `link_library_enriches_outbound_message_before_fanout`: given a message with
   `https://example.com/post`, the host calls `message.enrich.v1`, appends one
   typed `urn:waddle:plugin:link-library:0` payload, keeps the original body,
   and broadcasts one message.
3. `link_library_skips_duplicate_embed`: given a message that already contains
   a Link Library payload, enrichment returns zero new embeds and does not
   publish duplicate PubSub items.
4. `link_library_blocks_private_redirects`: given a public URL that redirects
   to `127.0.0.1`, the host returns `fetch_status=blocked`, no preview image,
   and a delivered original message.
5. `link_library_indexes_with_pubsub_event_shape`: publishing a link record
   uses `http://jabber.org/protocol/pubsub` and emits a notification in
   `http://jabber.org/protocol/pubsub#event` with item ID `{posted_at}:{link_id}`.
6. `link_library_sidebar_reads_only_granted_prefix`: a room sidebar request can
   read `links/index` for the installation prefix and receives `forbidden` for
   another plugin's node prefix.
7. `link_library_config_command_uses_data_form`: executing
   `waddle:link-library:configure` returns an XEP-0050 command with a
   `jabber:x:data` form containing host allowlist fields and no secret fields.

### Pub Quiz

Pub Quiz runs moderated quiz sessions in a room with timed questions, private
answer submission, scoring, and a public scoreboard.

Manifest capabilities:

- `command.handle.v1`
- `message.respond.v1`
- `pubsub.state.v1`
- `ui.surface.v1`
- `members.read.v1`

Required grants:

- `commands:execute:room` for `waddle:quiz:create`, `waddle:quiz:start`,
  `waddle:quiz:answer`, `waddle:quiz:scoreboard`, and `waddle:quiz:end`.
- `messages:write:room` for question, timer, and scoreboard messages.
- `pubsub:read:prefix` and `pubsub:write:prefix` under the quiz installation
  prefix.
- `members:read:room` for display names on scoreboards.

Optional grants:

- None.

Data model:

```text
QuizSession
├── quiz_id: QuizId
├── room_jid: BareJid
├── host_jid: BareJid
├── title: HumanText
├── status: draft | active | paused | completed | canceled
├── questions: Vec<QuestionId>
├── current_question: Option<QuestionId>
├── scoring: ScoringPolicy
├── created_at: Timestamp
└── updated_at: Timestamp

QuizQuestion
├── question_id: QuestionId
├── quiz_id: QuizId
├── prompt: HumanText
├── options: Vec<AnswerOption>
├── correct_option: AnswerOptionId
├── time_limit_seconds: u16
└── points: u16

QuizAnswer
├── answer_id: AnswerId
├── quiz_id: QuizId
├── question_id: QuestionId
├── respondent: BareJid
├── selected_option: AnswerOptionId
├── answered_at: Timestamp
└── accepted: bool

QuizScore
├── quiz_id: QuizId
├── participant: BareJid
├── points: u32
├── correct_count: u16
└── rank: u16
```

State nodes and events:

| Node suffix | Item ID | Payload namespace | Event |
|-------------|---------|-------------------|-------|
| `quizzes` | `{quiz_id}` | `urn:waddle:plugin:pub-quiz:0` | `quiz.created` |
| `quizzes/{quiz_id}/questions` | `{question_id}` | `urn:waddle:plugin:pub-quiz:0` | `quiz.question.added` |
| `quizzes/{quiz_id}/answers/{question_id}` | `{respondent_hash}` | `urn:waddle:plugin:pub-quiz:0` | `quiz.answer.submitted` |
| `quizzes/{quiz_id}/scores` | `{participant_hash}` | `urn:waddle:plugin:pub-quiz:0` | `quiz.score.updated` |
| `quizzes/{quiz_id}/events` | `{event_id}` | `urn:waddle:plugin:pub-quiz:0` | quiz lifecycle events |

Answer nodes are readable by the plugin and the quiz host only until scores are
finalized. Public score nodes expose participant display names only after
scoring.

UI surfaces:

- `composer-command`: `/quiz create`, `/quiz start`, `/quiz answer`,
  `/quiz scoreboard`, `/quiz end`.
- `message-inline`: question card with answer buttons for supporting clients.
- `room-sidebar`: current quiz status and scoreboard.
- `detail-modal`: quiz authoring and review.
- `admin-settings`: default time limit, allowed hosts, scoring policy.

Failure and fallback behavior:

- If the answer command is submitted after the question closes, return an
  XEP-0050 completed command with `note type='error'` and do not publish an
  answer item.
- If a participant submits twice, keep the first accepted answer unless the
  scoring policy explicitly allows updates.
- If rich question cards are unsupported, the plugin-authored message body
  contains the question and numbered options, with XEP-0428 marking the body as
  fallback for `urn:waddle:plugin:pub-quiz:0`.
- If member display lookup fails, scoreboards show stable participant handles
  derived from bare JIDs without leaking full resource JIDs.

Acceptance tests:

1. `pub_quiz_manifest_has_no_network_or_ai_grants`: the manifest declares quiz
   command, response, PubSub, UI, and member-read capabilities only.
2. `pub_quiz_create_command_returns_xep0050_data_form`: executing
   `waddle:quiz:create` returns a command in
   `http://jabber.org/protocol/commands` with a `jabber:x:data` form for title,
   time limit, and scoring policy.
3. `pub_quiz_start_publishes_question_and_fallback_body`: starting a quiz
   publishes the active question state, sends one room message with the custom
   quiz payload, includes a readable body, and includes XEP-0428 fallback for
   `urn:waddle:plugin:pub-quiz:0`.
4. `pub_quiz_answer_is_not_room_broadcast`: submitting an answer writes to the
   private answer state node and sends no groupchat message containing the
   selected option.
5. `pub_quiz_rejects_late_answer_without_state_mutation`: an answer after
   `question.closed_at` returns a command error note and leaves the answer node
   unchanged.
6. `pub_quiz_scoreboard_uses_member_grant_only`: scoreboard rendering uses
   `members:read:room` display metadata and does not request message history or
   user secrets.
7. `pub_quiz_pubsub_notifications_are_xep0060_conformant`: score updates are
   delivered as `pubsub#event` item notifications to authorized subscribers only.

### AI ChatBot

AI ChatBot responds to mentions and explicit commands using the
deployment-configured AI provider. The plugin never receives provider API keys
or user tokens.

Manifest capabilities:

- `command.handle.v1`
- `message.read-recent.v1`
- `message.respond.v1`
- `pubsub.state.v1`
- `ui.surface.v1`
- `ai.invoke.v1`

Required grants:

- `commands:execute:room` for `waddle:ai-chatbot:ask`,
  `waddle:ai-chatbot:configure`, and `waddle:ai-chatbot:forget-thread`.
- `messages:write:room` for bot responses.
- `messages:read:recent` with an explicit message-count and room scope for
  context retrieval.
- `pubsub:read:prefix` and `pubsub:write:prefix` under the chatbot
  installation prefix.
- `ai:invoke:deployment` for host-mediated model calls.

Optional grants:

- None for secrets. Optional context depth is a grant parameter, not a secret.

Data model:

```text
ChatBotConfig
├── bot_display_name: HumanText
├── system_prompt: HumanText
├── max_context_messages: u16
├── response_mode: mention_only | command_only | mention_and_command
├── allowed_rooms: Vec<BareJid>
└── safety_mode: strict | standard

ChatThread
├── thread_id: ChatThreadId
├── room_jid: BareJid
├── started_by: BareJid
├── root_message_id: Option<StableMessageId>
├── status: active | archived
└── last_turn_at: Timestamp

ChatTurn
├── turn_id: ChatTurnId
├── thread_id: ChatThreadId
├── requester: BareJid
├── prompt_message_id: StableMessageId
├── response_message_id: Option<StableMessageId>
├── status: queued | running | completed | failed | refused
├── context_degraded: bool
├── model_label: HumanText
└── created_at: Timestamp
```

State nodes and events:

| Node suffix | Item ID | Payload namespace | Event |
|-------------|---------|-------------------|-------|
| `config` | `default` | `urn:waddle:plugin:ai-chatbot:0` | `chatbot.config.updated` |
| `threads` | `{thread_id}` | `urn:waddle:plugin:ai-chatbot:0` | `chatbot.thread.created` |
| `threads/{thread_id}/turns` | `{turn_id}` | `urn:waddle:plugin:ai-chatbot:0` | `chatbot.turn.updated` |
| `events` | `{event_id}` | `urn:waddle:plugin:ai-chatbot:0` | chatbot lifecycle events |

UI surfaces:

- `bot-profile`: display bot capabilities, model label, and privacy note.
- `composer-command`: `/ask`, `/forget-thread`, `/ai-settings`.
- `message-inline`: response card with status, refusal, or generated answer.
- `room-sidebar`: active AI threads and recent failures for admins.
- `admin-settings`: prompt, response mode, context depth, and allowed rooms.

Failure and fallback behavior:

- If `ai:invoke:deployment` is unavailable, commands return a clear error note
  and mention triggers produce no response unless configured to send a short
  unavailable message.
- If recent message context cannot be read, the bot may answer with the prompt
  only and must mark the turn as `completed` with `context_degraded=true`.
- Recent message context must exclude ephemeral messages and messages from users
  whose AI-processing preference opts out of inclusion. If this removes all
  context, continue as prompt-only with `context_degraded=true`.
- If the provider refuses or fails, publish a `ChatTurn` with `status=refused`
  or `failed` and send a short human-safe message without raw provider output.
- If the bot loses `messages:read:recent`, it must continue only in
  command-only, prompt-only mode or disable itself according to configuration.

Acceptance tests:

1. `ai_chatbot_manifest_has_ai_grant_but_no_secret_fields`: the manifest
   declares `ai.invoke.v1`, no external provider key field, and no client-secret
   configuration.
2. `ai_chatbot_config_form_redacts_provider_details`: the admin settings
   command returns XEP-0004 fields for response mode and context depth, but not
   API keys, refresh tokens, or model provider secrets.
3. `ai_chatbot_mention_creates_turn_and_response`: a mention in an allowed room
   creates a `ChatTurn` with `queued`, invokes the host AI provider, publishes
   `completed`, and sends one plugin-authored room message.
4. `ai_chatbot_context_scope_is_bounded`: the host passes no more than
   `max_context_messages` from the configured room and never includes messages
   from another room.
5. `ai_chatbot_provider_failure_does_not_leak_secret`: a simulated provider
   error containing secret-looking text is redacted in the room message and
   admin error event.
6. `ai_chatbot_forget_thread_retracts_state_items`: executing
   `waddle:ai-chatbot:forget-thread` retracts the thread's turn items through
   XEP-0060 and leaves an audit event.
7. `ai_chatbot_unsupported_client_gets_plain_body`: a bot response with custom
   metadata includes a readable body and XEP-0428 fallback markers.
8. `ai_chatbot_respects_ai_context_exclusions`: recent context passed to
   `ai.invoke.v1` excludes ephemeral messages, opted-out users' messages, and
   messages outside the granted room scope.

### AI Dynamic Canvas

AI Dynamic Canvas creates and updates collaborative visual canvases through
typed canvas state, optional host AI generation, and XEP-0363-hosted assets.

Manifest capabilities:

- `command.handle.v1`
- `message.respond.v1`
- `pubsub.state.v1`
- `ui.surface.v1`
- `ai.invoke.v1`
- `file.upload.v1`

Required grants:

- `commands:execute:room` for `waddle:canvas:create`,
  `waddle:canvas:prompt`, `waddle:canvas:apply-patch`, and
  `waddle:canvas:export`.
- `messages:write:room` for canvas creation and update messages.
- `pubsub:read:prefix` and `pubsub:write:prefix` under the canvas
  installation prefix.
- `ai:invoke:deployment` for prompt-to-patch generation.
- `upload:request:room` for generated preview images or exports.

Optional grants:

- None for user secrets. Export storage uses Waddle's upload service only.

Data model:

```text
CanvasDocument
├── canvas_id: CanvasId
├── room_jid: BareJid
├── owner: BareJid
├── title: HumanText
├── status: draft | active | locked | archived
├── current_version: CanvasVersion
├── preview_url: Option<Url>
├── created_at: Timestamp
└── updated_at: Timestamp

CanvasPatch
├── patch_id: CanvasPatchId
├── canvas_id: CanvasId
├── base_version: CanvasVersion
├── author: BareJid
├── operation: CanvasOperation
├── status: proposed | applied | rejected | failed
├── prompt: Option<HumanText>
└── created_at: Timestamp

CanvasOperation
├── add_layer(CanvasLayer)
├── update_layer(CanvasLayerId, LayerProperties)
├── remove_layer(CanvasLayerId)
├── reorder_layers(Vec<CanvasLayerId>)
└── set_viewport(CanvasViewport)
```

State nodes and events:

| Node suffix | Item ID | Payload namespace | Event |
|-------------|---------|-------------------|-------|
| `canvases` | `{canvas_id}` | `urn:waddle:plugin:ai-dynamic-canvas:0` | `canvas.created` |
| `canvases/{canvas_id}/patches` | `{patch_id}` | `urn:waddle:plugin:ai-dynamic-canvas:0` | `canvas.patch.proposed` |
| `canvases/{canvas_id}/versions` | `{version}` | `urn:waddle:plugin:ai-dynamic-canvas:0` | `canvas.patch.applied` |
| `canvases/{canvas_id}/assets` | `{asset_id}` | `urn:waddle:plugin:ai-dynamic-canvas:0` | `canvas.asset.uploaded` |
| `events` | `{event_id}` | `urn:waddle:plugin:ai-dynamic-canvas:0` | canvas lifecycle events |

UI surfaces:

- `message-inline`: compact canvas preview with current version and status.
- `detail-modal`: full canvas editor rendered by first-party Waddle UI from
  typed canvas state.
- `composer-command`: `/canvas create`, `/canvas prompt`, `/canvas export`.
- `room-sidebar`: canvas list and recent patch activity.
- `admin-settings`: who can apply patches, AI generation enabled flag, export
  size limits.

Failure and fallback behavior:

- If AI generation fails, keep the canvas unchanged, publish
  `canvas.patch.failed`, and show the prompt as failed in the detail modal.
- If a patch base version is stale, reject the patch with a conflict result and
  return the latest version ID.
- If XEP-0363 upload slot allocation fails, keep the typed canvas state and send
  a message with no preview image.
- If a client does not support the canvas surface, the plugin-authored message
  body contains the canvas title and latest version, with XEP-0428 fallback.

Acceptance tests:

1. `ai_canvas_manifest_uses_host_ai_and_upload_grants`: the manifest declares
   `ai.invoke.v1` and `file.upload.v1`, but no provider secret fields and no
   arbitrary filesystem grants.
2. `ai_canvas_create_publishes_document_and_message`: creating a canvas writes
   one `CanvasDocument` item, sends one room message with custom canvas payload,
   readable body, and XEP-0428 fallback.
3. `ai_canvas_prompt_generates_patch_without_mutating_document`: submitting a
   prompt creates a `CanvasPatch` in `proposed` state before any version update.
4. `ai_canvas_apply_patch_checks_base_version`: applying a patch with an old
   base version returns a conflict command result and does not publish a new
   version item.
5. `ai_canvas_upload_uses_xep0363_slot`: exporting a preview requests a slot
   from the XEP-0363 service and stores only the returned GET URL in the asset
   state.
6. `ai_canvas_ai_failure_is_visible_but_redacted`: a provider failure marks the
   patch `failed`, publishes an admin error without raw provider payload, and
   leaves the prior canvas version active.
7. `ai_canvas_detail_modal_reads_pubsub_state_only`: opening the canvas modal
   retrieves document, version, patch, and asset items from the granted prefix
   and performs no direct plugin HTTP request.

### Decision Polls

Decision Polls lets a room create lightweight decisions, collect votes, and
publish final outcomes. It uses Waddle-specific poll payloads because there is
no official XEP poll payload, while using XEP-0050, XEP-0004, and XEP-0060 for
commands, forms, and state transport.

Manifest capabilities:

- `command.handle.v1`
- `message.respond.v1`
- `pubsub.state.v1`
- `ui.surface.v1`
- `members.read.v1`

Required grants:

- `commands:execute:room` for `waddle:poll:create`, `waddle:poll:vote`,
  `waddle:poll:close`, and `waddle:poll:results`.
- `messages:write:room` for poll cards and outcome messages.
- `pubsub:read:prefix` and `pubsub:write:prefix` under the poll installation
  prefix.
- `members:read:room` when displaying voter names for non-secret polls.

Optional grants:

- None.

Data model:

```text
DecisionPoll
├── poll_id: PollId
├── room_jid: BareJid
├── creator: BareJid
├── question: HumanText
├── options: Vec<PollOption>
├── status: draft | open | closed | canceled
├── ballot_visibility: public | anonymous
├── selection_mode: single | multiple
├── closes_at: Option<Timestamp>
├── created_at: Timestamp
└── closed_at: Option<Timestamp>

PollVote
├── vote_id: VoteId
├── poll_id: PollId
├── voter: BareJid
├── selected_options: Vec<PollOptionId>
├── cast_at: Timestamp
└── supersedes: Option<VoteId>

PollResult
├── poll_id: PollId
├── totals: Vec<PollOptionTotal>
├── total_voters: u32
├── decided_option: Option<PollOptionId>
└── computed_at: Timestamp
```

State nodes and events:

| Node suffix | Item ID | Payload namespace | Event |
|-------------|---------|-------------------|-------|
| `polls` | `{poll_id}` | `urn:waddle:plugin:decision-polls:0` | `poll.created` |
| `polls/{poll_id}/votes` | `{voter_hash}` | `urn:waddle:plugin:decision-polls:0` | `poll.vote.cast` |
| `polls/{poll_id}/results` | `current` | `urn:waddle:plugin:decision-polls:0` | `poll.results.updated` |
| `polls/{poll_id}/events` | `{event_id}` | `urn:waddle:plugin:decision-polls:0` | poll lifecycle events |

For anonymous polls, `PollVote` payloads are readable only by the plugin and
room admins; member-visible results expose aggregate counts only.

UI surfaces:

- `composer-command`: `/poll create`, `/poll vote`, `/poll close`,
  `/poll results`.
- `message-inline`: poll card with options, status, and aggregate totals.
- `room-sidebar`: open polls and recent decisions.
- `detail-modal`: vote history for public polls or aggregate-only history for
  anonymous polls.
- `admin-settings`: default duration, who can close polls, anonymous allowed.

Failure and fallback behavior:

- If a vote is cast after close, return a command error note and leave vote and
  result nodes unchanged.
- If a voter changes a vote while the poll is open, publish one replacement item
  at item ID `{voter_hash}` and include `supersedes`.
- If the client does not support poll cards, the body lists question, options,
  and close time, with XEP-0428 fallback for
  `urn:waddle:plugin:decision-polls:0`.
- If member lookup fails for a public poll, display stable handles from bare JID
  localparts, not full JIDs or resources.

Acceptance tests:

1. `decision_polls_manifest_uses_no_network_or_ai_grants`: the manifest
   declares command, response, PubSub, UI, and member-read capabilities only.
2. `decision_polls_create_uses_xep0050_form`: creating a poll starts with an
   ad-hoc command and submits question, options, selection mode, and visibility
   through `jabber:x:data`.
3. `decision_polls_open_poll_message_has_fallback`: opening a poll sends one
   room message with custom poll payload, readable body, and XEP-0428 fallback.
4. `decision_polls_vote_replaces_by_voter_hash`: a second vote from the same
   bare JID overwrites the same PubSub item ID and updates results once.
5. `decision_polls_anonymous_results_hide_voters`: member-visible results for
   an anonymous poll contain totals and total voter count, but no bare JIDs,
   full JIDs, resources, or display names.
6. `decision_polls_late_vote_is_rejected`: voting after `closed_at` returns an
   XEP-0050 error note and does not update vote or result nodes.
7. `decision_polls_close_publishes_final_result`: closing a poll publishes the
   final `PollResult`, sends a plugin-authored outcome message, and retracts or
   marks any open-vote UI state as closed.

## API Endpoints

These endpoints are the original bot-framework sketch and are not the
implementation path for the sample plugins above. Sample plugins must use the
shared plugin framework, XMPP-native commands, PubSub state, and host grants
described in this RFC section.

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
