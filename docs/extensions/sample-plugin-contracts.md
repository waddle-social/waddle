# Waddle Extension Sample Plugin Contracts

This is the implementation companion to
`docs/superpowers/plans/2026-04-27-waddle-extension-xmpp-protocol.md`.
It intentionally defines exactly three sample plugins for the first extension
framework slice. Each sample uses the typed `urn:waddle:extension:1`
framework path directly; there is no compatibility bridge or legacy plugin
contract.

## Shared Contract

- Runtime: server-side WASM component from OCI by digest.
- Protocol: XMPP-native control through `extensions.<domain>`.
- UI: declarative Waddle-rendered descriptors only; no iframes, plugin HTML,
  plugin JavaScript, plugin CSS, WebViews, or client-run WASM.
- State: XEP-0060 PubSub nodes; users do not publish directly to plugin nodes.
- Actions: XEP-0050 `urn:waddle:extension:1:invoke`.
- Messages: server-authored `<extensions xmlns='urn:waddle:extension:1'>`
  envelope only; plugin payloads live under `<enrichment><payload>`.
- Secrets: server-owned profiles only. Clients never receive or submit provider
  API keys, OAuth refresh tokens, webhook secrets, or model credentials.

## OCI Manifest Minimum

Every sample plugin fixture should include these manifest fields:

```json
{
  "framework": "urn:waddle:extension:1",
  "witWorld": "waddle:extension@1.0.0#world:waddle-extension",
  "pluginId": "link-board",
  "payloadNamespace": "urn:waddle:link-board:1",
  "wasmDigest": "sha256:...",
  "capabilities": [],
  "permissions": [],
  "routes": [],
  "actions": []
}
```

For these sample plugin fixtures, the installer must reject mutable tags,
missing digests, digest mismatches, unknown capabilities, unknown permissions,
and payload namespaces outside the entries declared by each plugin manifest.

## Plugin Matrix

| Plugin | Plugin ID | Payload Namespace | Primary Surface |
| --- | --- | --- | --- |
| Link Board | `link-board` | `urn:waddle:link-board:1` | Declarative task board with OpenGraph link enrichment. |
| Standard AI Chatbot | `ai-chatbot` | `urn:waddle:ai-chatbot:1` | Assistant-style typed enrichment with launchable follow-ups. |
| Decision Polls | `decision-polls` | `urn:waddle:decision-polls:1` | Lightweight decisions with private votes and public aggregates. |

## Link Board

Capabilities:
`message.enrich`, `launch`, `commands`, `pubsub.publish`,
`artifact.reference`, `ui.declarative`.

Permissions:
`message.enrich`, `pubsub.publish`, `net.fetch.opengraph`.

Visible output:

- Link previews are message enrichments; save/create-task launches persist
  extension-owned PubSub items.

Actions:

- `save-link`: launched from a message enrichment; writes a link item.
- `create-task`: launched from a message enrichment or board route; writes a
  task item under `tasks:{board-id}`.

Tests:

- Link message gets one framework envelope.
- OpenGraph metadata is fetched only by server permission.
- Board route rejects iframe/HTML/JS/CSS descriptors.
- Direct user PubSub publish to board nodes is forbidden.

## Standard AI Chatbot

Capabilities:
`commands`, `launch`, `message.enrich`, `message.observe`.

Permissions:
`message.observe`.

Visible output:

- Assistant answers are message enrichments; follow-up launches return another
  visible command result.

Actions:

- `ask`: command action behind `/ai`.
- `ask-followup`: launch action from an assistant answer enrichment.

Tests:

- Mention, DM, reply, and `/ai` trigger an assistant answer enrichment.
- Unrelated room messages do not trigger a passive answer.
- No provider credential or model configuration is exposed through the sample
  extension API.

## Decision Polls

Capabilities:
`commands`, `launch`, `message.enrich`, `pubsub.publish`, `ui.declarative`.

Permissions:
`pubsub.publish`.

Routes:

- `poll-results`: member view for aggregate results.
- `poll-admin`: admin view for close/reopen actions and private vote audit.

Actions:

- `create-poll`: command action behind `/poll`.
- `vote`: launch action from per-option buttons.
- `close-poll`: command or scheduled action.

Tests:

- Vote launch writes to admin-only votes node.
- Member-visible results omit voter JIDs unless poll mode is public-voter.
- Closing a poll prevents further vote launches without rewriting old messages.
