# Remove Multi-Server App Support and Non-XMPP REST Routes

## Summary

Remove app support for arbitrary Waddle server selection and eliminate Waddle-specific REST CRUD routes from `waddle-server`. Keep only HTTP surfaces required to operate XMPP, OAuth-for-XMPP, file upload, and operations.

OAuth is “XMPP-native” here via XEP-0493/SASL OAuthBearer, but OAuth still requires HTTP authorization/token endpoints by design.

## Key Changes

- `chat/`: remove custom homeserver UI, `waddle_server` redirect handling, active-server localStorage, and session maps keyed by server URL. Use the configured default server only.
- `apps/apple/`: remove editable/persisted server URL support and per-server session maps. Keep a single configured server URL and single session slot.
- Remove app usage of `WaddleApi`/`WaddleAPIClient` for Waddle/channel/member/message REST. Keep auth/session HTTP only where needed for XMPP login/session bootstrap.
- Replace existing app behavior with XMPP equivalents where already implemented: canonical waddle discovery, channel discovery, channel creation command, MAM/history, inbox, presence, DMs, and upload slot discovery.
- Remove or hide UI/actions that only work through REST and do not yet have XMPP support, including REST-backed waddle update/delete, channel update/delete, member management, and HTTP user search.
- `server`: stop merging and then remove REST route modules for `/v1/space`, `/v1/waddles...`, `/v1/channels...`, `/v1/permissions...`, `/v1/users/search`, and REST message listing. Move any reusable non-route logic into internal services if WebSocket/XMPP handlers still need it.
- Delete obsolete compatibility handlers for old multi-waddle routes rather than preserving aliases.

## Remaining HTTP Route Inventory

Document these in a concise repo/server doc; location can be `server/docs/specs/http-routes.md`.

- `GET /xmpp-websocket`: RFC 7395 XMPP over WebSocket C2S transport.
- `GET /.well-known/host-meta`, `GET /.well-known/host-meta.json`: XEP-0156 XMPP WebSocket discovery.
- `GET /.well-known/oauth-authorization-server`: OAuth metadata used by XMPP OAuth clients.
- `GET /api/auth/xmpp/authorize`, `POST /api/auth/xmpp/token`: XEP-0493/OAuth token issuance for XMPP SASL OAuthBearer.
- `GET /api/auth/providers`, `GET /api/auth/start`, `GET /api/auth/callback`, `GET /api/auth/session`, `POST /api/auth/logout`: browser/app session bootstrap until the apps are fully XMPP-native auth only.
- `POST /api/auth/device/start`, `POST /api/auth/device/poll`, `GET/POST /api/auth/device/verify`: Apple/CLI device authorization for XMPP sessions.
- `PUT/OPTIONS /api/upload/:slot_id`, `GET /api/files/:slot_id/:filename`: XEP-0363 HTTP upload/download after an XMPP slot request.
- `GET /health`, `/healthz`, `/ready`, `/readyz`, `/metrics`, `/api/v1/health`: operational liveness/readiness/metrics.
- Remove `/api/v1/server-info`; clients already use XEP-0092 software version over XMPP.

Also document separate non-chat apps if desired: `website` waitlist routes and `colony` OAuth provider routes are outside the Waddle chat/server protocol surface.

## Test Plan

- `chat/`: run `bun test`, `bun run lint`, and `bun run build`.
- `server/`: run targeted Rust tests for WebSocket auth, XEP-0156, XEP-0363, XEP-0493/auth, MAM, discovery, and channel commands; then run the full relevant cargo test set.
- `apps/apple/`: run the existing `xcodebuild` target after removing REST client/types and project references.
- Add regression checks that no app code references `/v1/`, `waddle_server`, active-server storage keys, `WaddleApi`, or `WaddleAPIClient`.
- Add server route tests asserting removed REST paths return 404 and retained XMPP-required routes still work.

## Assumptions

- “No non-XMPP HTTP routes” means no Waddle domain REST CRUD, not no HTTP at all.
- OAuth/device/session HTTP routes stay for now because they are required to obtain authenticated XMPP sessions.
- Missing XMPP management features should be removed from UI rather than silently falling back to REST.
- Existing dirty worktree changes are user work and must be preserved; implementation should edit around them without reverting unrelated changes.
