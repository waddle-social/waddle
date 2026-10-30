# HTTP Route Inventory

Waddle chat behavior is XMPP-native. HTTP is retained only where the protocol
stack, authentication bootstrap, upload transfer, or operations require it.

## Retained Routes

- `GET /ws`: RFC 7395 XMPP over WebSocket client transport.
- `GET /.well-known/host-meta`: XEP-0156 WebSocket discovery.
- `GET /.well-known/host-meta.json`: XEP-0156 WebSocket discovery.
- `GET /.well-known/oauth-authorization-server`: OAuth metadata for XMPP OAuth clients.
- `GET /api/auth/xmpp/authorize`: XEP-0493 OAuth authorization.
- `POST /api/auth/xmpp/token`: XEP-0493 OAuth token issuance.
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
- `GET /health`: Liveness. Fails only for states a restart fixes; never
  touches the database, lineage or claims. `503` with
  `"error": "runtime_stalled"`: the runtime heartbeat has not ticked for
  10 s. `503` with `"error": "terminally_fenced"`: the node took a clustering
  fatal fence (while serving or while re-registering) and its node-lease loop
  finished the terminal demotion and exited, so it can never serve again in
  this process. This includes clustering's own shutdown finding the node
  lease unavailable, which takes a fatal fence. The demotion has no deadline
  of its own; a wedged publication guard or actor delays this signal. `503`
  with `"error": "clustering_stopped"`: the clustering scope stopped while
  the process keeps running (for example the swarm's keypair-slot lease
  self-fenced) and the node-lease loop exited, with no fatal fence; nothing
  re-serves the node in this process. `clustering_stopped` is never reported
  while the process itself is stopping; `terminally_fenced` can be, when the
  shutdown drain finds the lease unavailable (harmless then). A clustered
  Postgres outage longer than the 30 s lease TTL ends in one of these two
  reasons, so liveness restarts the pod.
- `GET /healthz`: Liveness alias.
- `GET /ready`: Readiness. Database pool health, per-store lineage and the
  node lifecycle (claims/self-fence, drain), all bounded so the answer
  arrives within 1.5 s; an unreachable database returns `503`.
- `GET /readyz`: Readiness alias.
- `GET /metrics`: Prometheus metrics.
- `GET /api/v1/health`: Detailed health.

## Removed Waddle REST Routes

The server does not expose Waddle-domain CRUD over HTTP. Space, channel,
member, permission, message, user-search, and server-info behavior must flow
through XMPP or internal services.
