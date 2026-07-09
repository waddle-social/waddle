# HTTP Route Inventory

Waddle chat behavior is XMPP-native. HTTP is retained only where the protocol
stack, authentication bootstrap, upload transfer, or operations require it.

## Retained Routes

- `GET /ws`: RFC 7395 XMPP over WebSocket client transport.
- `GET /.well-known/host-meta`: XEP-0156 WebSocket discovery.
- `GET /.well-known/host-meta.json`: XEP-0156 WebSocket discovery.
- `GET /api/auth/providers`: Browser/app auth provider discovery.
- `GET /api/auth/start`: Browser/app auth start.
- `GET /api/auth/callback`: Browser/app auth callback.
- `GET /api/auth/session`: Browser/app session bootstrap.
- `POST /api/auth/logout`: Browser/app logout.
- `POST /api/auth/device/start`: Device authorization start.
- `POST /api/auth/device/poll`: Device authorization polling.
- `GET /api/auth/device/verify`: Device authorization verification page.
- `POST /api/auth/device/verify`: Device authorization verification submit.
- `GET /api/calendar/community-feed-url?community_jid=:jid`: Authenticated
  helper returning a signed iCalendar subscription URL for the deployment's
  xCal community events PubSub node. It accepts the browser session from the
  `waddle_session` cookie or `X-Waddle-Session-Id` request header and returns
  `Cache-Control: no-store`.
- `GET /api/calendar/community/:token/events.ics`: Read-only `text/calendar`
  projection of the xCal community events PubSub node for external calendar
  clients. The token is a bearer subscription secret stored by calendar
  clients; the route serves data only while the backing PubSub node remains
  public/open. Subscription tokens are community-scoped and non-expiring; all
  authenticated users receive the same URL for a community, and rotating the
  server `session_key` mass-invalidates existing calendar subscriptions. The
  route returns `Cache-Control: no-store`, scans at most the latest 10,000
  published PubSub rows, emits up to 1,000 feed-eligible calendar items after
  filtering RSVP-only sibling rows from that bounded window, and it does not
  expose Waddle CRUD or control semantics.
- `PUT /api/upload/:slot_id`: XEP-0363 upload transfer after an XMPP slot request.
- `OPTIONS /api/upload/:slot_id`: CORS preflight for XEP-0363 upload transfer.
- `GET /api/files/:slot_id/:filename`: XEP-0363 download transfer.
- `GET /health`: Liveness.
- `GET /healthz`: Liveness alias.
- `GET /ready`: Readiness.
- `GET /readyz`: Readiness alias.
- `GET /metrics`: Prometheus metrics.
- `GET /api/v1/health`: Detailed health.

## Removed Waddle REST Routes

The server does not expose Waddle-domain CRUD over HTTP. Space, channel,
member, permission, message, user-search, and server-info behavior must flow
through XMPP or internal services.

Waddle also does not advertise XEP-0493 OAuth Client Login. The former OAuth
metadata, XMPP authorization, and XMPP token routes were removed until Waddle
has a complete registered-client, scope, PKCE, revocation, and cluster-safe
authorization-server contract. SASL OAUTHBEARER remains available over secure
transports for already-issued Waddle session tokens.
