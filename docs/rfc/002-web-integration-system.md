# RFC-002: Web Integration System

**Status:** Proposed

**Author:** System

**Created:** 2025-09-30

## Abstract

This RFC defines Waddle's web integration system as an extension-framework
capability, not as a separate worker, event bus, GraphQL API, or direct message
writer.

The first concrete provider is a single general-purpose GitHub extension backed
by a GitHub App. GitHub webhooks provide realtime external provider ingress.
All Waddle-side control, configuration, state, routing, and user-visible output
remain XMPP-native through `extensions.<domain>`.

## Motivation

Communities discuss external content from many sources:

- RSS feeds for blogs, podcasts, and news.
- YouTube channels for videos, livestreams, and shorts.
- GitHub activity for pull requests, releases, issues, discussions, and CI.

The integration system should reduce manual posting while preserving the core
Waddle rule: integrations behave like installed XMPP-native extensions. They
must not bypass room authorization, MAM, stanza IDs, message references, or the
extension permission model.

V1 ships the GitHub extension path first because GitHub App installation gives
Waddle a clear provider identity, explicit repository permissions, webhook
subscriptions, installation IDs, and short-lived server-side API tokens.

## XMPP and Extension Baseline

Implementation must review the local XEP sources in `./xeps` before changing
wire behavior. These XEPs define the protocol shapes used by this RFC:

- XEP-0030 Service Discovery for discovering `extensions.<domain>`, extension
  commands, and PubSub nodes.
- XEP-0128 Service Discovery Extensions for metadata forms on disco info.
- XEP-0004 Data Forms for admin configuration forms.
- XEP-0050 Ad-Hoc Commands for install, configuration, test, and sync actions.
- XEP-0060 Publish-Subscribe for extension config, routing state, delivery
  state, dedupe ledgers, and recent errors.
- XEP-0313 MAM and XEP-0297 Stanza Forwarding for archived extension-authored
  messages.
- XEP-0359 Stanza IDs for stable message references.
- XEP-0372 References when a message annotates body URLs as data references.

Official XEP namespaces must only be used with their exact XEP-defined wire
shape. Waddle-specific provider semantics belong in Waddle-owned namespaces and
typed extension values.

## Architecture

```
External provider
  GitHub App webhooks / GitHub REST or GraphQL API
        |
        v
Provider ingress
  verify signature, parse provider payload, dedupe delivery
        |
        v
GitHub extension runtime
  typed provider event, config lookup, permission checks
        |
        v
XMPP-native effects
  XEP-0060 state updates, host.message.send, declarative enrichment
        |
        v
Waddle clients
  normal room messages, MAM replay, extension-rendered views
```

HTTP is allowed only at provider boundaries and for explicitly granted
server-side provider fetches. It is not a Waddle control API. Users do not
configure integrations, trigger actions, or mutate extension state through
provider webhook routes.

The current GitHub extension replaces the deleted legacy GitHub enricher. It is
the single place for GitHub behavior:

- GitHub Actions failure alerts in V1.
- Releases, pull requests, issues, discussions, and GitHub link/activity
  surfaces in later slices.
- Shared installation, repository selection, routing, permission, and delivery
  state for all GitHub use cases.

## GitHub Extension Contract

The extension is installed and advertised through the Waddle extension
framework:

| Field | Value |
| --- | --- |
| Plugin ID | `github` |
| Display name | `GitHub` |
| Bot JID | `github@extensions.<domain>` |
| Framework namespace | `urn:waddle:extension:1` |
| Payload namespace | `urn:waddle:web-integration:1` |

Do not revive the legacy `urn:waddle:github:0` namespace. Do not introduce new
`urn:waddle:github:*` namespaces. Any compatibility bridge for old GitHub
payloads must wrap client-visible output in the framework envelope before it
reaches clients.

The GitHub extension manifest declares only the capabilities it uses:

- `commands` for XEP-0050 admin and operator actions.
- `pubsub.publish` for durable extension-owned state.
- `host.message.send` for bot-authored room messages.
- `outbound.http.request` for GitHub API calls through an allowlist containing
  `https://api.github.com`.
- `ui.declarative` and `message.enrich` when a slice exposes rich message
  cards or route views.

Secrets are server-owned. The GitHub App private key, webhook secret, optional
client secret, and installation token cache must never appear in XEP-0050
forms, PubSub payloads visible to clients, message payloads, route descriptors,
or browser-delivered configuration.

## GitHub App Provider Model

Waddle owns a GitHub App registration. Repository owners install the app on the
repositories they want connected to Waddle. The app subscribes only to the
events needed by enabled extension features.

V1 required provider inputs:

- App ID or slug as non-secret server config.
- App private key through a server-owned secret file.
- Webhook secret through a server-owned secret file.
- GitHub App webhook URL pointing at Waddle provider ingress.
- Repository permissions sufficient for subscribed events and optional API
  enrichment.

The provider ingress handler must:

- Accept only GitHub App webhook deliveries.
- Verify `X-Hub-Signature-256` against the configured webhook secret.
- Require `X-GitHub-Event` and `X-GitHub-Delivery`.
- Parse the GitHub `installation.id`, repository ID, repository full name, and
  event-specific payload.
- Drop the raw JSON after parsing it into typed Rust values.
- Deduplicate by `X-GitHub-Delivery` before dispatching extension effects.
- Return quickly after validation and dispatch, without exposing extension
  internals to the provider.

When the extension needs GitHub API data, the server generates a short-lived
installation access token for the relevant installation. Tokens are scoped by
the GitHub App installation and must not be persisted as long-lived Waddle
state.

## Extension State

GitHub configuration and delivery state live in XEP-0060 PubSub nodes under
`extensions.<domain>`. Node names should follow the existing framework style and
use canonical server IDs, not display names.

Required state categories:

- Installations: GitHub installation ID, account identity, selected
  repositories, and enabled Waddle contexts.
- Routes: repository ID or `owner/repo`, event class, destination room JID, and
  enabled flag.
- Delivery ledger: GitHub delivery ID, event type, repository ID, outcome, and
  timestamp for dedupe and diagnostics.
- Sync cursors: per-installation or per-repository cursor state for future
  fallback reconciliation.
- Recent errors: provider parsing, permission, API, routing, and message-send
  failures.

Provider state must be typed before it reaches PubSub. Do not store arbitrary
provider JSON blobs as durable extension state unless a future slice defines a
typed archived-payload value with explicit retention rules.

## Admin Commands

The GitHub extension exposes XEP-0050 commands discovered through
`extensions.<domain>`.

Required V1 commands:

- `github:list-installations`: show installed GitHub App accounts visible to
  the requester.
- `github:configure-repository`: choose a GitHub installation/repository,
  destination room, and enabled event classes.
- `github:set-enabled`: enable or disable a configured repository route.
- `github:test-alert`: send a synthetic alert through the normal extension
  message path to prove routing and authorization.
- `github:sync-installation`: refresh installation and repository metadata from
  GitHub using a server-side installation token.

All forms use XEP-0004. Hidden `FORM_TYPE` values and field names must follow
the extension framework conventions. Forms may reference an installation or
repository by opaque ID, but they must not request or display provider secrets.

## V1 GitHub Actions Failure Alerts

V1 implements GitHub Actions failure alerts in the GitHub extension. This is the
first use case for the general extension, not a separate CI-alert plugin.

Subscribed GitHub App webhook events:

- `workflow_run`
- `check_run`
- `installation`
- `installation_repositories`

The extension emits an alert only when:

- the route is enabled for the installation and repository,
- the event type is enabled for the route,
- `action` is `completed`, and
- `conclusion` is `failure`, `timed_out`, or `cancelled`.

All other conclusions and actions are ignored for V1.

Each alert message includes:

- repository full name,
- workflow or check name,
- branch,
- short SHA,
- conclusion, and
- URL to the failed run or check.

Messages are sent as normal XMPP groupchat messages from the GitHub extension's
bot identity. They must pass through the same room authorization, stanza ID,
MAM, message reference, and client rendering paths as other extension-authored
messages.

## Future Provider Slices

Future GitHub slices should extend the same GitHub extension:

- Release announcements.
- Pull request activity.
- Issue activity.
- Discussion activity.
- GitHub link enrichment and previews.
- Fallback reconciliation for missed webhook deliveries.

RSS and YouTube remain part of the broader web integration product direction,
but they should be implemented as extension-framework capabilities with the
same XMPP-native rules rather than by reviving worker/Event Bus/direct-write
integration primitives.

## Security Considerations

- Provider webhook ingress must verify `X-Hub-Signature-256`; SHA-1 signatures
  are not sufficient for new code.
- Provider ingress is not a Waddle control plane. It cannot configure routes,
  install extensions, trigger user actions, or mutate Waddle state except
  through typed extension events after authorization.
- GitHub App credentials, webhook secrets, and installation tokens stay on the
  server and are never exposed to clients.
- Outbound HTTP from the extension is limited by granted capability and allowed
  origins.
- Delivery IDs are stored for dedupe before posting user-visible messages.
- Rate limits, retry policy, and failure recording are extension-owned state,
  not provider-controlled behavior.
- Direct database writes to message tables are forbidden for provider output.
  The extension must use the host message path.
- Client-rendered GitHub views use Waddle declarative descriptors only; no
  provider JavaScript, iframes, plugin DOM, plugin CSS, or client-run WASM.

## Test Plan

Provider ingress tests:

- Accept a valid `X-Hub-Signature-256` signature.
- Reject missing, malformed, or invalid signatures.
- Reject payloads without `X-GitHub-Event`, `X-GitHub-Delivery`,
  installation ID, or repository identity.
- Deduplicate repeated delivery IDs.
- Parse `workflow_run` and `check_run` completed failure payloads into typed
  GitHub extension events.
- Ignore success, skipped, neutral, in-progress, and unsupported events.

Extension protocol tests:

- GitHub commands are discoverable through XEP-0030 and XEP-0050.
- Admin forms use XEP-0004 with the expected `FORM_TYPE`.
- Config, routing state, delivery ledger, sync cursors, and errors use XEP-0060
  nodes on `extensions.<domain>`.
- No new GitHub payload uses `urn:waddle:github:0` or `urn:waddle:github:*`.

Message behavior tests:

- A matching failure event posts exactly one message to the configured room.
- Duplicate delivery IDs do not post duplicate messages.
- Disabled routes and unknown repositories do not post messages.
- Posted messages are archived in MAM and replay with stanza IDs intact.
- Alerts are sent by `github@extensions.<domain>` or that bot's room occupant
  JID, not by a synthetic system user.

Regression guards:

- Existing checks that prevent `github-enricher` from returning to GitOps stay
  in place.
- Any legacy GitHub compatibility bridge wraps output in
  `<extensions xmlns='urn:waddle:extension:1'>` before clients see it.

## References

- [GitHub App webhooks](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/using-webhooks-with-github-apps)
- [GitHub App permissions](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/choosing-permissions-for-a-github-app)
- [GitHub App installation authentication](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-as-a-github-app-installation)
- [GitHub webhook events and payloads](https://docs.github.com/en/webhooks/webhook-events-and-payloads)
- [RSS 2.0 Specification](https://www.rssboard.org/rss-specification)
- [YouTube Data API](https://developers.google.com/youtube/v3)
