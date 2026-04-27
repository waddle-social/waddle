# Waddle Extension Sample Plugin Contracts

This is the implementation companion to
`docs/superpowers/plans/2026-04-27-waddle-extension-xmpp-protocol.md`.
It intentionally defines exactly five sample plugins for the first extension
framework slice. The deployed `github-enricher` module is a legacy
compatibility bridge for GitHub link enrichment, not one of these sample
plugins.

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
  "pluginId": "links-task-board",
  "payloadNamespace": "urn:waddle:links-task-board:1",
  "wasmDigest": "sha256:...",
  "capabilities": [],
  "permissions": [],
  "routes": [],
  "actions": []
}
```

For these sample plugin fixtures, the installer must reject mutable tags,
missing digests, digest mismatches, unknown capabilities, unknown permissions,
and payload namespaces outside the five entries below. The legacy
`github-enricher` compatibility module remains separately deployable by
digest-pinned GitOps configuration until these sample plugins have published
artifacts.

## Plugin Matrix

| Plugin | Plugin ID | Payload Namespace | Primary Surface |
| --- | --- | --- | --- |
| Links Task Board | `links-task-board` | `urn:waddle:links-task-board:1` | Declarative task board with OpenGraph link enrichment. |
| Pub Quiz | `pub-quiz` | `urn:waddle:pub-quiz:1` | Host-led quiz rounds with launchable answer buttons. |
| Standard AI Chatbot | `ai-chatbot` | `urn:waddle:ai-chatbot:1` | Plain assistant bot using bounded MAM context. |
| AI Assistant Dynamic Canvas | `ai-assistant-canvas` | `urn:waddle:ai-assistant-canvas:1` | Server-owned AI canvas generation and immutable render artifacts. |
| Decision Polls | `decision-polls` | `urn:waddle:decision-polls:1` | Fifth useful example: lightweight decisions with private votes and public aggregates. |

## Links Task Board

Capabilities:
`message.enrich`, `launch`, `commands`, `pubsub.publish`,
`artifact.reference`, `ui.declarative`.

Permissions:
`message.enrich`, `pubsub.publish`, `net.fetch.opengraph`.

Routes:

- `board`: reads boards, links, tasks, and OpenGraph cache PubSub nodes.
- `link-detail`: reads one link item and offers save/create-task actions.

Actions:

- `save-link`: launched from a message enrichment; writes a link item.
- `create-task`: launched from a message enrichment or board route; writes a
  task item under `tasks:{board-id}`.

Tests:

- Link message gets one framework envelope.
- OpenGraph metadata is fetched only by server permission.
- Board route rejects iframe/HTML/JS/CSS descriptors.
- Direct user PubSub publish to board nodes is forbidden.

## Pub Quiz

Capabilities:
`commands`, `launch`, `bot.respond`, `pubsub.publish`, `ui.declarative`.

Permissions:
`bot.send.message`, `pubsub.publish`.

Routes:

- `quiz-host`: admin view for active game, current question, and close action.
- `leaderboard`: member view for current aggregate scores.

Actions:

- `start-game`: command action behind `/quiz start`.
- `answer`: launch action from answer buttons.
- `close-question`: host or scheduled action.

Tests:

- `/quiz start` sends a normal XMPP groupchat bot message.
- Answer launch writes admin-only submission state.
- Member leaderboard omits answer secrets until a question closes.

## Standard AI Chatbot

Capabilities:
`commands`, `launch`, `bot.respond`, `message.observe`, `ai.invoke`.

Permissions:
`message.observe`, `mam.read.context`, `bot.send.message`, `ai.invoke`.

Routes:

- `chat`: optional transcript/config view backed by PubSub run summaries.

Actions:

- `ask`: command action behind `/ai`.
- `ask-followup`: launch action from a bot answer.

Tests:

- Mention, DM, reply, and `/ai` trigger the bot.
- Unrelated room messages do not trigger a passive answer.
- AI profile selection is a server profile id, not a credential.

## AI Assistant Dynamic Canvas

Capabilities:
`commands`, `launch`, `bot.respond`, `artifact.reference`, `ai.invoke`,
`pubsub.publish`, `ui.declarative`.

Permissions:
`bot.send.message`, `pubsub.publish`, `artifact.write`, `ai.invoke`.

Routes:

- `canvas`: member view for canvas state and latest immutable render.
- `render-history`: member view of previous render artifacts.

Actions:

- `create-canvas`: command action behind `/canvas`.
- `remix`: launch action from an existing canvas message.

Tests:

- User prompt/style enters through XEP-0050 only.
- WASM calls server `ai.invoke`; it cannot call provider HTTP directly.
- Render output is written to immutable artifact storage by digest.
- No client or user secret appears in message payloads, PubSub items, route
  descriptors, or static artifacts.

## Decision Polls

Capabilities:
`commands`, `launch`, `bot.respond`, `pubsub.publish`, `ui.declarative`.

Permissions:
`bot.send.message`, `pubsub.publish`.

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
