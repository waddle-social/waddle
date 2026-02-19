# Website Auth Status

Website authentication is intentionally disabled in this release.

- All `/api/auth/*` endpoints return `410 Gone`.
- ATProto OAuth metadata/JWKS endpoints are removed.
- Use server auth broker endpoints under `/v2/auth/*` from `waddle-server`.

No auth runtime wiring is maintained in `colony/website`.
