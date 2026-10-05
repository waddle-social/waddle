# Research: the MCP server and MCP Events surface a dot can use

Resolves [#1916](https://github.com/waddle-social/waddle/issues/1916), part of map [#1914](https://github.com/waddle-social/waddle/issues/1914).
Researched 2026-10-05 against primary sources. Every claim cites its source. "Inference" marks conclusions that are not stated by a source.

## Gist

Waddle has to be a **stateless MCP `2026-07-28` Streamable HTTP server** and an OAuth 2.1 resource server. It also has to implement OpenAI's **webhook-only MCP Events** profile:

- `events` in the `server/discover` capabilities;
- `events/list`, `events/subscribe` and `events/unsubscribe`;
- durable subscriptions;
- a signed verification challenge;
- signed deliveries using Standard Webhooks;
- a 256 KiB body cap;
- no retry on 410 or 413;
- SSRF-safe egress.

No Rust crate implements Events. Use `rmcp` ≥ 3.5 for the core protocol and build the Events layer in Waddle.

This surface **must be host-side**. The WASM extension ABI has no inbound request path, and its outbound HTTP cannot meet the callback requirements.

A dot never acts on an event unless its owner has created an event-triggered task in ChatGPT.

## 1. Transport and protocol

| Fact | Source |
|---|---|
| A ChatGPT plugin endpoint must "Support the MCP streamable HTTP transport" and "Respond at a stable URL, typically ending in `/mcp`". A public submission needs a "stable, publicly reachable HTTPS endpoint". | https://developers.openai.com/plugins/build/mcp-server#deploy-the-endpoint |
| "MCP Events in ChatGPT requires MCP 2.0 (protocol version `2026-07-28`). Configure your server in your plugin and provide persistent subscription storage and outbound HTTPS access to callback URLs." | https://developers.openai.com/plugins/build/mcp-events#before-you-start |
| `2026-07-28` is the released, latest spec. GitHub release `2026-07-28` (2026-07-28, not a prerelease); `schema/2026-07-28/schema.ts:30` has `LATEST_PROTOCOL_VERSION = "2026-07-28"`. | https://github.com/modelcontextprotocol/modelcontextprotocol/releases |
| **Stateless.** "remove the `initialize`/`notifications/initialized` handshake. Every request now carries its protocol version and client capabilities in `_meta` (`io.modelcontextprotocol/protocolVersion`, `io.modelcontextprotocol/clientCapabilities`)". Also: "Remove protocol-level sessions and the `Mcp-Session-Id` header". | https://modelcontextprotocol.io/specification/2026-07-28/changelog |
| "Servers **MUST** implement `server/discover`". Its result carries `supportedVersions`, `capabilities`, `serverInfo` and `instructions`. | https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning#protocol-version-negotiation, https://modelcontextprotocol.io/specification/2026-07-28/server/discover |
| Every POST carries `MCP-Protocol-Version`, which must match `_meta`; a mismatch returns 400 `HeaderMismatch`. `Mcp-Method` is required, and `Mcp-Name` is required for `tools/call`, `resources/read` and `prompts/get`. GET and DELETE return `405`. Any `Mcp-Session-Id` is ignored. `Last-Event-ID` is ignored ("streams are not resumable"). "Servers **MUST** validate the `Origin` header". | https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http |
| "All results now carry a required `resultType` field". MRTR (`InputRequiredResult`) replaces server-to-client requests. | https://modelcontextprotocol.io/specification/2026-07-28/changelog |
| "OpenAI-registered MCP servers require multi-round-trip requests (MRTR)". | https://developers.openai.com/plugins/build/extensions#rich-forms |
| `tools/list` "**MUST NOT** vary per-connection … **MAY** vary by the authorization presented on the request". | https://modelcontextprotocol.io/specification/2026-07-28/server/tools#capabilities |
| Cross-call state uses "explicit, server-minted handles passed as ordinary tool arguments (SEP-2567)". | https://modelcontextprotocol.io/specification/2026-07-28/changelog |

**Tools and `_meta`.** Source: https://developers.openai.com/plugins/reference and https://developers.openai.com/plugins/plugin-guidelines.

- **One operation per tool.** "Expose each model-callable operation as a separate tool … Do not use … a generic executor". Each tool needs a non-empty description, an `inputSchema`, and an `outputSchema` whenever it returns `structuredContent`.
- **Annotations.** `readOnlyHint`, `destructiveHint` and `openWorldHint` are required as explicit booleans. Posting messages is `readOnlyHint: false`: "Use `false` for … write-style outbound actions such as posting messages" (plugin-guidelines#correct-annotation).
- **Client-sent `_meta` is not authorization.** The client sends `openai/subject`, `openai/session`, `openai/organization`, `openai/locale`, `openai/userAgent` and `openai/userLocation`. The docs say "servers should never rely on them for authorization decisions and must tolerate their absence".
- **Status text.** Optional `openai/toolInvocation/invoking` and `openai/toolInvocation/invoked`, each ≤ 64 characters.
- **Profile tool for multiple accounts** (https://developers.openai.com/plugins/build/auth#support-multiple-accounts). The tool is authenticated and read-only, takes empty arguments, and is marked `_meta["openai/profile"]: true`. It returns `{id, name?, email?, nickname?}` with `additionalProperties: false`, in `structuredContent` and also as JSON text. The `id` must "Remain the same for the same profile across token refresh, reconnect, and scope upgrades … Never be reassigned". The tool is optional: "Users can connect multiple accounts without a profile tool".
- **`openai/` prefix.** It is a valid, non-reserved `_meta` prefix under MCP's naming rules, although it skips the reverse-DNS SHOULD (https://modelcontextprotocol.io/specification/2026-07-28/basic#meta). This is inference from the rule text.

**Authorization.** The OAuth topology belongs to the sibling ticket. These are only the resource-server facts this surface depends on:

- **Resource server role.** The MCP server is an OAuth 2.1 resource server.
- **Protected Resource Metadata.** The server MUST publish RFC 9728 metadata, either at `/.well-known/oauth-protected-resource` or via `WWW-Authenticate: Bearer resource_metadata=…` on 401.
- **Audience.** It "**MUST** validate that access tokens were issued specifically for them as the intended audience". It "**MUST NOT** pass through the token" it received.
- **Token transport.** Tokens are never accepted in the query string.
- Source for the four points above: https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization
- **ChatGPT client registration.** ChatGPT prefers CIMD, uses PKCE with `S256` (required in authorization-server metadata), sends `resource=` on both the authorization and token requests, and requires RFC 9207 `iss`. Machine-to-machine grants are unsupported. Source: https://developers.openai.com/plugins/build/auth#custom-auth-with-oauth-21

## 2. MCP Events (OpenAI webhook profile)

**Status.** Events are not in MCP core. No `events` capability or method exists in `2026-07-28`, and no official `ext-` repository covers them.

- **The spec ChatGPT implements** is a draft in an incubation repository: https://github.com/modelcontextprotocol/experimental-ext-triggers-events/blob/main/docs/design-sketch-proposal.md. Its header reads "Status: Draft proposal … 2026-02-19".
- **SEP-3415 "Events Extension"** opened as a draft on 2026-10-05: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/3415. It moves the capability under `extensions["io.modelcontextprotocol/events"]`, while ChatGPT expects a top-level `capabilities.events`.
- **Expect wire churn.**
- **What ChatGPT supports.** "ChatGPT supports webhook delivery and callback verification from the draft MCP Events specification … Polling, streaming, and the draft's `gap` and `terminated` control notifications are not supported" (https://developers.openai.com/plugins/build/mcp-events#before-you-start).

The facts below come from https://developers.openai.com/plugins/build/mcp-events unless another source is named. All quotes were checked against the live `.md` page.

- **Advertise.** Add `"events": {}` to `capabilities` in the `server/discover` result. Implement the three methods "on the same authenticated MCP endpoint as your tools" (#advertise-event-support).
- **`events/list`.** Each event has `name`, `description`, `delivery: ["webhook"]`, `inputSchema` (the subscription arguments) and `payloadSchema` (the `data` object). Pagination uses `nextCursor`. "Return only events the connected account is allowed to discover". The use-case table names exactly `message.created`, filtered by `channel_id` (#define-an-event).
- **`events/subscribe`.** Params are `{name, arguments, delivery: {mode: "webhook", url, secret: "whsec_…"}, cursor, ttlMs?}`.
  - Before accepting, the server must:
    1. authorize the user for the event and its arguments;
    2. validate the arguments;
    3. require "a `whsec_` signing secret whose base64 value decodes to 24–64 bytes";
    4. verify the callback;
    5. store the owner, filters, URL, secret and expiry.
  - "Derive a deterministic subscription ID from the authenticated principal, callback URL, event name, and arguments". Compare arguments as canonical JSON. Subscribe is idempotent.
  - The result is `{id, refreshBefore, cursor, truncated}`.
  - ChatGPT generates the secret. The server never does (sketch #webhook-security).
- **Callback verification.**
  - Before any data, POST `{"type":"verification","challenge":"<single-use>"}`. Sign it with its own `webhook-id`, `webhook-timestamp`, `webhook-signature` and `X-MCP-Subscription-Id`.
  - Require a 2xx response that echoes `{"challenge": …}`. Compare the echo in constant time.
  - On failure, return JSON-RPC `-32015` `CallbackEndpointError` with `data.reason`, for example `challenge_failed` or `timeout` (#verify-the-callback).
- **Egress rules.** "Require HTTPS for callbacks. Resolve and validate destination addresses at connection time, then connect to the validated address while preserving the original hostname for TLS verification. Block private, local, and other non-public addresses, and do not follow redirects." The same rules apply to verification requests.
- **Delivery envelope.** The body is `{eventId, name, timestamp, data, cursor}`.
  - Keep `eventId` stable across retries.
  - `data` must match `payloadSchema`.
  - "a top-level `type` identifies a protocol control notification".
  - "For large records, send a summary and expose a read tool … Treat comments and other user-authored text as data; do not add instructions telling the model how to behave inside the event payload" (#send-an-event).
- **Signing.**
  - Headers: `webhook-id` (equal to `eventId`), `webhook-timestamp` (Unix seconds), `webhook-signature` and `X-MCP-Subscription-Id`.
  - Serialize the body once, and sign and send exactly those bytes (#sign-the-request).
  - The signature is `v1,` + base64(HMAC-SHA256(secret, id + "." + ts + "." + body)), where the secret is the base64-decoded value after `whsec_` (sketch #webhook-security).
- **Limits and retries.**
  - "Send one event per request, with a complete request body no larger than 256 KiB (262,144 bytes)."
  - "Retry transient failures with exponential backoff and bounded attempts. Preserve the event ID and generate a fresh signing timestamp and signature for each attempt. Do not retry deliveries that return `410` or `413`."
  - "Events can arrive out of order." (#handle-delivery-responses)
  - A 410 rejects that one delivery and does **not** end the subscription (sketch). The sketch suggests about 3–5 attempts within 10–15 minutes. OpenAI's sample uses a 10 s timeout and `redirect: "error"`.
- **Lifecycle.**
  - "Retain subscription state for the lifetime you grant, including across server restarts. Recheck the user's access during the subscription's lifetime and stop delivery if access is revoked."
  - ChatGPT refreshes by calling `events/subscribe` again before `refreshBefore`.
  - Omitting `ttlMs` means the server default. `ttlMs: null` requests no expiry.
  - When a refresh supplies a new secret, sign with both the old and new secrets during a rotation window.
  - Replay: `cursor: null` "for event types without replay; events missed during an interruption cannot be recovered".
  - Unsubscribe is idempotent and authorized (#manage-subscriptions).
- **Testing.** Test that the resulting actions do "not create a feedback loop" (#test-in-chatgpt).

## 3. Dots and the Slack semantics to mirror

- **Identity.** A dot is a personal, always-on agent with a handle such as `@yourname-dot`. It "can use supported plugins installed and enabled for your account, with their connected accounts and existing permissions" (https://learn.chatgpt.com/docs/dots).
- **Slack entry points.** "In Slack, you can send a direct message or add your dot to a channel and mention it in a thread" (https://learn.chatgpt.com/docs/dots/channels#slack).
- **Default audience.** "By default, your dot responds to you in Slack. You can instruct it to engage with others … Your dot can message you privately to ask what you're comfortable sharing before it replies in a channel" (same page).
- **Owner-only direction.** "Only the owner can direct their dot through a Slack direct message or supported channel mention. Messages from other people do not start work. A dot may use other participants' messages as context when its owner brings it into a Slack thread … Context carried across channels does not authorize sharing with a new audience" (https://learn.chatgpt.com/docs/enterprise/dots-admin-guide).
- **Events do not start work by themselves.** A monitoring task is explicit: "When a connected service supports event monitoring, you can instead ask your dot to respond to a specific event … Connecting Slack or another source alone doesn't create a monitoring task" (https://learn.chatgpt.com/docs/dots/tasks-and-memory#event-monitoring). The only statement about delivery is that ChatGPT "receives the event in the subscribed chat and follows the user's instructions for how to respond" and "can group separately delivered events into one task run" (mcp-events#how-it-works, #handle-delivery-responses).
- **Not documented** (unverified, so the prototype ticket must observe it):
  - which dot conversation an event lands in;
  - how the payload is rendered to the model;
  - how batching is configured for dots.
- **No public API.** No public dot API is documented. The only third-party entry points are plugins (MCP tools) and MCP Events (https://learn.chatgpt.com/docs/dots, https://learn.chatgpt.com/docs/enterprise/cloud-local-access).

Inference: in Slack, the dot is woken natively by the ChatGPT Slack app. In Waddle, the **only** push path is an MCP event, so "owner @mention or DM wakes the dot" works only if the owner has a standing event task in ChatGPT, such as "when I mention you in Waddle, reply in that thread". Onboarding must create or explain that task. Waddle cannot create it.

## 4. Rust MCP crates

Checked on crates.io and GitHub on 2026-10-05.

| crate | latest | 2026-07-28 | Streamable HTTP server | Events |
|---|---|---|---|---|
| `rmcp` (official, modelcontextprotocol/rust-sdk) | 3.5.1 (2026-10-05); 32.4M downloads total; ~4k★ | yes, `LATEST = V_2026_07_28` since 3.0.0 (`crates/rmcp/src/model.rs:169-201`) | tower `StreamableHttpService`, mounts in axum 0.8; stateless for 2026-07-28; Origin/Host allowlists (`transport/streamable_http_server/tower.rs:78-140`) | **none** |
| `rust-mcp-sdk` 2.0.0 (community) | 2026-08-27; 301k downloads | yes, 2026-07-28 only | axum/actix wrappers, some server-side auth | none |
| `pmcp` 2.22.7 | 2026-10-04 | opt-in only | yes | none |
| `turbomcp` 3.6.0 | 2026-10-01 | only in the 4.0 alphas | yes | none |
| `ultrafast-mcp`, `mcpr`, `mcp-protocol-sdk`, `mcp-sdk-rs` | stale or archived | no | – | none |

Permalinks: `https://github.com/modelcontextprotocol/rust-sdk/blob/79437f291b2c44053d00dcd5db969fd0cca7c887/<path>`.

- **The events capability cannot be advertised as-is.** rmcp's `ServerCapabilities` is `#[non_exhaustive]` with fixed fields: `experimental`, `extensions`, `logging`, `completions`, `prompts`, `resources` and `tools` (`crates/rmcp/src/model/capabilities.rs:223-243`, verified). It cannot emit a top-level `"events": {}` today. The options are an upstream PR, or a response rewrite on `server/discover` only.
- **Custom methods are supported.** They arrive through `ServerHandler::on_custom_request` (`crates/rmcp/src/handler/server.rs:552`).
- **No WASM HTTP server.** None of these crates provides a Streamable HTTP server inside a WASM component. rmcp's `wasm32-wasip2` example is stdio-only (`examples/wasi`).
- **Waddle has no MCP dependency today.** Its stack (axum 0.8.9, tokio 1.52.3, hyper 1.9) matches rmcp's (`server/Cargo.toml:31-39`, `Cargo.lock`).
- **Prior art.** Hand-rolled Events in Rust on axum: https://github.com/everruns/everruns/blob/main/crates/server/src/api/mcp_endpoint/events.rs

## 5. Host-side or inside the WASM extension

Evidence from Waddle at `d17e4b61b`:

- **No inbound request path.** The extension world exports only `lifecycle` and `framework` (`server/wit/waddle-extension.wit:844-853`). `handle-event` receives only these `extension-event` variants: `room-message-observe`, `message-hook`, `command`, `launch`, `provider-webhook` (`waddle-extension.wit:522-528`). The extension cannot serve or answer an HTTP or JSON-RPC request.
- **Provider webhook ingress is inbound-only and asynchronous.** `POST /webhooks/providers/{provider_id}/{plugin_id}` (`server/crates/waddle-server/src/server/routes/extension_webhooks.rs:29-36`):
  - verifies one HMAC secret per provider, taken from operator env (`extension_webhooks.rs:113-168`, `:564-585`);
  - flattens the JSON (`:587-632`);
  - returns `202` before dispatch (`:226`, `:327-341`);
  - persists only PubSub effects (`:434`).
  - It cannot return a JSON-RPC result body, carries no per-user principal, and has no OAuth.
- **Outbound HTTP is limited** (`runtime.http-request`, `waddle-extension.wit:831-842`, `:772-792`):
  - only GET and POST, with a string body;
  - the response has `status` and `body` but **no headers**;
  - allowed origins are a static per-module operator allowlist (`server/crates/waddle-extensions/src/config.rs:37-40`, enforced at `src/runtime/http.rs:53-61`), while ChatGPT supplies callback URLs dynamically per subscription;
  - it is HTTPS-only with no redirects (`http.rs:17`, `:41-46`), which is good;
  - there is no resolve-then-pin private-address block;
  - each invocation is capped at 32 requests and 30 s (`server/crates/waddle-extensions/src/config/limits.rs:51-54`);
  - each invocation is a one-shot `handle-event` with no durable retry schedule of its own.
- **Room observation is operator-scoped.** The scope is `Rooms(Vec<BareJid>)` or `AllHostedRooms` (`server/crates/waddle-extensions/src/types/observation.rs:35-46`), filtered in `manager/room_observation.rs:16-30`. There is no per-owner or per-occupancy scope. Observers cannot send messages (`runtime/host_state.rs:106-115`).
- **Hard Rules forbid these paths today.** "Do not add REST, GraphQL, webhook, or browser postMessage control paths" (`docs/superpowers/plans/2026-04-27-waddle-extension-xmpp-protocol.md:52-53`). "User clients never receive or submit user/provider secrets" (`:34-37`). The map already plans the ADR amendment for this.

**Conclusion.** The following must be **host-side** in `waddle-server`, as a native axum route plus a background delivery worker:

- the MCP transport;
- OAuth resource-server checks;
- the subscription store;
- callback verification;
- signing;
- SSRF-safe egress;
- the retry outbox.

A WASM module could still own tool semantics or payload shaping later. That would need new WIT: an inbound `mcp-tool-call` event with a synchronous typed result and a per-owner principal in `InvocationContext`. That is a design-ticket option, not a prerequisite.

Inference: the default `http_max_request_bytes = 256 * 1024` (`limits.rs:52`) matches the Events cap only by coincidence. Nothing ties the two together.

## 6. Hard constraints for the design tickets

1. **Protocol baseline.** Implement stateless MCP `2026-07-28` Streamable HTTP:
   - one POST `/mcp` endpoint;
   - `server/discover`;
   - per-request `_meta` version and capabilities;
   - `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` validation;
   - 405 on GET and DELETE;
   - no sessions;
   - `resultType` on every result;
   - MRTR;
   - Origin validation.

   Any multi-call state is an explicit handle. Since there is no session affinity, any replica can serve any request.
2. **Host-side placement.** The MCP endpoint, OAuth resource-server checks, subscription store and delivery worker live in `waddle-server`, not in a WASM module (§5). Changing that requires new WIT, not a reuse of `provider-webhook` or `runtime.http-request`.
3. **Durable subscriptions in the shared database.** Each row holds:
   - the owner principal;
   - the agent JID;
   - the event name;
   - canonical arguments;
   - the callback URL;
   - the `whsec_` secret (24–64 bytes);
   - the previous secret, during rotation;
   - expiry.

   The ID is deterministic from (principal, URL, name, canonical arguments). Subscribe and unsubscribe are idempotent. The rows survive restarts. Secrets never reach clients or the PubSub projection.
4. **Authorization at subscribe and at every delivery.** A subscription may name only conversations the agent can currently see: rooms where it is a joined occupant, plus the owner DM. Delivery stops when the agent leaves, is kicked, is disabled, or the room forbids personal agents ("Recheck the user's access … stop delivery if access is revoked").
5. **Callback egress.**
   - HTTPS only.
   - Resolve, then reject private, loopback and link-local addresses.
   - Connect to the pinned IP using the original SNI.
   - Follow no redirects.
   - Apply bounded timeouts.
   - Apply all of this to verification too.
   - Activate a subscription only after a constant-time challenge echo; otherwise return `-32015`.
6. **Delivery.**
   - One event per POST, with the serialized body ≤ 262,144 bytes. Measure after serialization and shrink the payload (§7) rather than fail.
   - Standard Webhooks signature over the exact bytes, plus `X-MCP-Subscription-Id`.
   - Delivery comes from a durable outbox, written in the same commit as the message's archive entry or a successor of it.
   - Bounded exponential retries that keep `eventId` and re-sign every attempt.
   - Never retry 410 or 413, and do not cancel the subscription on 410.
   - Do not assume ordering.
7. **No feedback loop.** Never emit an event for a message whose sender is the agent itself, including its replies and corrections.
8. **Payloads are data.** Keep user text inside `data`. Add no instructions to the model. Send summaries plus a read tool for anything large.
9. **Isolate the Events wire shape.** It is a draft. OpenAI's `capabilities.events` and SEP-3415's `extensions["io.modelcontextprotocol/events"]` already disagree. Keep the Events types in one Waddle module. With `rmcp`, advertising `events` needs an upstream PR or a `server/discover` response rewrite.
10. **Activation is explicit in ChatGPT.** The dot responds only to events its owner has bound to a task in ChatGPT. The enable flow must tell the owner how to create that task, and the prototype must confirm how the dot sees an event.
11. **Profile identity is stable.** If a profile tool is shipped, its `id` is the owner's stable Waddle account identity, never reassigned. A user-chosen handle cannot be that id, because handles can change.
12. **Tool annotations are honest.** Reads use `readOnlyHint: true`. Posting or replying uses `readOnlyHint: false` and `openWorldHint: false`. A destructive tool such as retract uses `destructiveHint: true`.

## 7. Proposed `message.created` event for a MUC room

This is one event for both rooms and the owner DM. The subscription arguments select the scope. The trigger semantics follow the map: only the owner's @mention or the owner's DM is "directed". All other messages in occupied rooms are context.

### `events/list` entry

```json
{
  "name": "message.created",
  "description": "A new message in a Waddle room your agent has joined, or in your direct conversation with your agent. Messages from other people are context only; owner_directed is true only when you mentioned your agent or messaged it directly.",
  "delivery": ["webhook"],
  "inputSchema": {
    "type": "object",
    "properties": {
      "room_jid": {
        "type": "string",
        "description": "Bare JID of one room the agent occupies. Omit to cover every room the agent occupies and your direct conversation with it."
      },
      "only_owner_directed": {
        "type": "boolean",
        "default": true,
        "description": "When true, deliver only messages where you mention your agent or message it directly. Set false to monitor every message in the room."
      }
    },
    "additionalProperties": false
  },
  "payloadSchema": {
    "type": "object",
    "properties": {
      "conversation": {
        "type": "object",
        "properties": {
          "kind": { "type": "string", "enum": ["room", "direct"] },
          "jid": { "type": "string" },
          "name": { "type": ["string", "null"] }
        },
        "required": ["kind", "jid", "name"],
        "additionalProperties": false
      },
      "message_id": { "type": "string", "description": "XEP-0359 stanza-id assigned by the room/archive; pass to reply and read tools." },
      "sender": {
        "type": "object",
        "properties": {
          "nick": { "type": "string" },
          "is_owner": { "type": "boolean" }
        },
        "required": ["nick", "is_owner"],
        "additionalProperties": false
      },
      "owner_directed": { "type": "boolean" },
      "text": { "type": "string", "maxLength": 16000 },
      "text_truncated": { "type": "boolean" },
      "reply_to_message_id": { "type": ["string", "null"], "description": "XEP-0461 reply target, if this message is a reply." },
      "thread_id": { "type": ["string", "null"], "description": "XEP-0201 thread, if any." },
      "sent_at": { "type": "string", "format": "date-time" }
    },
    "required": ["conversation", "message_id", "sender", "owner_directed", "text", "text_truncated", "reply_to_message_id", "thread_id", "sent_at"],
    "additionalProperties": false
  }
}
```

### Delivered event

```json
{
  "eventId": "evt_3f9c…",
  "name": "message.created",
  "timestamp": "2026-10-05T14:02:11Z",
  "data": {
    "conversation": { "kind": "room", "jid": "eng@groups.waddle.social", "name": "eng" },
    "message_id": "01J9ZC4W6Q1M3R0B5T7Y2K8XHN",
    "sender": { "nick": "oyr", "is_owner": true },
    "owner_directed": true,
    "text": "@alfred-dot can you summarise the deploy thread?",
    "text_truncated": false,
    "reply_to_message_id": null,
    "thread_id": null,
    "sent_at": "2026-10-05T14:02:10Z"
  },
  "cursor": null
}
```

### Rules

- **`eventId`.** `evt_` + hex(SHA-256(subscription id ‖ archive stanza-id)). It is unique per subscription and stable across retries. The stanza-id is already the archive identity (`room-message-source.stanza-id`, `waddle-extension.wit:503-512`).
- **`owner_directed`.** It is true when the sender's bare JID is the owner and either the message is in the owner DM or it mentions the agent. A non-owner mention is `false`, mirroring "Messages from other people do not start work".
- **Excluded messages:**
  - messages from the agent itself (constraint 7);
  - messages from before the agent joined;
  - corrections and retractions (`revision > 0`) in v1. A later `message.updated` can add them.
- **Size budget.** Cap `text` at 16,000 characters and set `text_truncated`; worst-case UTF-8 is ~64 KB, well under 256 KiB. The dot fetches context and full text with read tools: room history from join onward, and fetch by `message_id`. No surrounding-context window is embedded.
- **Privacy.** Report only the occupant nick and `is_owner`. Real JIDs of other occupants are omitted, because the room's anonymity setting decides who may see them.
- **Replay.** `cursor: null` in v1. A MAM stanza-id cursor (XEP-0313 `after`, with `truncated: true` beyond retention) is the natural upgrade.
- **Reply path.** The matching write tool takes `{conversation_jid, text, reply_to_message_id}` and sends as the agent's JID with an XEP-0461 reply. This keeps the XMPP-native rule: the HTTP surface maps onto XMPP operations and does not replace them.

### Open questions this leaves for the design tickets

- Should `only_owner_directed: false` (full-room monitoring) ship in v1? Every room message would then leave Waddle for OpenAI, which the room may not expect even though the agent is a visible occupant.
- Should a single subscription without `room_jid` follow new rooms as the agent joins them? This draft says yes, which needs constraint 4 to be checked per delivery.
- What does the dot actually see and do on arrival? This is not documented (§3) and is for the prototype ticket to observe.
