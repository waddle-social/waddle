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
