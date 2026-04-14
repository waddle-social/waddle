# Call Stack Operator Runbook

Operator runbook for the current media/call implementation in `waddle-server`.

## Scope (current implementation)

- Call APIs:
  - `GET /v1/media/backend`
  - `POST /v1/media/sessions` (create call or join by room)
  - `GET /v1/media/calls?session_id=...&room_id=...`
  - `GET /v1/media/calls/:call_id?session_id=...`
  - `POST /v1/media/calls/:call_id/bootstrap?session_id=...`
  - `POST /v1/media/calls/:call_id/leave?session_id=...`
- Active call state is in-memory (`ActiveCallRegistry`). It is not persisted and is reset on process restart.
- Session auth is required via `session_id` query parameter on call routes.

## Required env/config

### Required for authenticated call operations

| Variable | Required | Notes |
|---|---|---|
| `WADDLE_BASE_URL` | Yes | Base HTTP URL; also default for media public URL fallback. |
| `WADDLE_SESSION_KEY` | Yes | Required for stable server-side session encryption/decryption across restarts. |
| `WADDLE_AUTH_PROVIDERS_JSON` | Yes (for non-native provider flows) | Provider registry config used to issue authenticated sessions. |
| `WADDLE_MEDIA_BACKEND` | Yes | `webrtc-rs-sfu` for enabled call stack; `disabled` returns `503 media_disabled`. |

### Media backend configuration

| Variable | Required when `WADDLE_MEDIA_BACKEND=webrtc-rs-sfu` | Notes |
|---|---|---|
| `WADDLE_MEDIA_PUBLIC_BASE_URL` | Recommended | Public absolute URL used to build `join_url`; defaults to `WADDLE_BASE_URL`. |
| `WADDLE_MEDIA_SFU_SIGNALING_PATH` | Optional | Default: `/v1/media/sfu`. |
| `WADDLE_MEDIA_SFU_ROOM_PREFIX` | Optional | Default: `waddle`; must be alphanumeric / `-` / `_`. |
| `WADDLE_MEDIA_SFU_ICE_SERVERS_JSON` | Recommended | JSON array of ICE servers, e.g. `["stun:stun.l.google.com:19302"]`. |

### Helm notes

- `WADDLE_SESSION_KEY` and `WADDLE_AUTH_PROVIDERS_JSON` are normally in Secret values.
- Media env vars are not first-class chart keys; set them via `config.extraEnv` or `containerExtraEnv`.

## Rollout checklist

1. Set/verify required env vars above.
2. Confirm `WADDLE_MEDIA_BACKEND=webrtc-rs-sfu` in target environment.
3. Roll deploy/restart and wait for readiness (`/ready` and `/api/v1/health`).
4. Validate call backend endpoint returns enabled backend.
5. Run API lifecycle checks (create, list, detail, bootstrap, leave) with a valid `session_id`.
6. Confirm metrics/log ingestion after synthetic lifecycle calls.
7. Watch `waddle_call_failures_total` for unexpected growth during rollout.

## Verification steps

Assume:

- `BASE_URL=https://<server>`
- `SESSION_ID=<valid-session-uuid>`

### 1) Backend + health

```bash
curl -fsS "$BASE_URL/health"
curl -fsS "$BASE_URL/api/v1/health"
curl -fsS "$BASE_URL/v1/media/backend"
```

Expected: backend reports `webrtc-rs-sfu` for enabled call stack.

### 2) Create (or room-join) call session

```bash
curl -fsS -X POST \
  "$BASE_URL/v1/media/sessions?session_id=$SESSION_ID" \
  -H 'content-type: application/json' \
  -d '{"room_id":"ops-smoke","role":"publisher"}'
```

Expected: `201`, returns `call_id`, `media_session.join_url`, `media_session.session_id`.

### 3) List and detail active call

```bash
curl -fsS "$BASE_URL/v1/media/calls?session_id=$SESSION_ID&room_id=ops-smoke"
curl -fsS "$BASE_URL/v1/media/calls/<call_id>?session_id=$SESSION_ID"
```

Expected: call exists with participant count >= 1.

### 4) Bootstrap join by `call_id`

```bash
curl -fsS -X POST \
  "$BASE_URL/v1/media/calls/<call_id>/bootstrap?session_id=$SESSION_ID" \
  -H 'content-type: application/json' \
  -d '{"role":"subscriber"}'
```

Expected: `200`, returns updated `call` and `media_session`.

### 5) Leave

```bash
curl -fsS -X POST \
  "$BASE_URL/v1/media/calls/<call_id>/leave?session_id=$SESSION_ID"
```

Expected: `200` with `removed=true` for active participant leave.

## Troubleshooting matrix

| Symptom | API/Error | Likely cause | Operator action |
|---|---|---|---|
| Create fails before call starts | `400 invalid_media_request` (`room_id`, `role`, or `session_id` validation) | Invalid request fields | Use UUID `session_id`; `room_id` charset `[A-Za-z0-9_-]`; role `publisher`/`subscriber`. |
| Create/join fails | `503 media_disabled` | `WADDLE_MEDIA_BACKEND=disabled` | Set backend to `webrtc-rs-sfu` and redeploy. |
| Join-by-room fails under load | `429 rate_limited` | In-memory abuse limits hit (create: 30/min/user) | Back off and retry after window reset; investigate abusive client loops. |
| Bootstrap fails | `404 call_not_found` or `400 invalid_media_request` (`call_id`) | Unknown/invalid call ID, stale call after restart, malformed UUID | Refresh call list and re-bootstrap with current `call_id`; recreate call after restart. |
| Bootstrap fails under load | `429 rate_limited` | Join limit hit (bootstrap/join: 60/min/user) | Back off and retry. |
| Leave returns non-removal | `200 removed=false` | Participant was already absent or different session user | Treat as idempotent success for cleanup; verify correct session identity. |
| Leave fails under load | `429 rate_limited` | Leave limit hit (90/min/user) | Back off and retry. |
| Any route fails auth | `404 session_not_found`, `401 session_expired`, or `500 auth_error` | Missing/expired/invalid session | Re-authenticate to obtain fresh `session_id`; verify session key consistency across pods. |
| Registry/internal failures | `500 call_registry_unavailable` | In-process call registry task unavailable | Restart pod/process and re-run lifecycle verification. |
| SFU join URL invalid/misrouted | `400 invalid_media_request` from media backend | Invalid `WADDLE_MEDIA_PUBLIC_BASE_URL`/room prefix/path config | Fix media env vars; verify absolute public URL and signaling path. |

## Observability references

### Metrics endpoint

- `GET /metrics` (Prometheus text format)

### Key call metrics

- `waddle_call_starts_total`
- `waddle_call_joins_total`
- `waddle_call_leaves_total`
- `waddle_active_calls`
- `waddle_call_failures_total{operation,reason}`
- `waddle_call_operation_duration_seconds_sum{operation}`
- `waddle_call_operation_duration_seconds_count{operation}`

Operation labels currently emitted: `create`, `bootstrap`, `leave`.

### Key log signals

- Success info events:
  - `event="call_create_succeeded"`
  - `event="call_bootstrap_succeeded"`
  - `event="call_leave_processed"`
- Failure warning event:
  - message: `call lifecycle operation failed`
  - fields: `operation`, `failure_reason`, `call_id`, `room_id`, `participant_id`
