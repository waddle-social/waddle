# SASL2 negotiation

`WADDLE_XMPP_SASL2` selects whether the RFC 7395 WebSocket listener offers
XEP-0388 Extensible SASL Profile (SASL2). The Helm chart renders it from
`xmpp.sasl2`; production sets it explicitly in
`infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml`.

| Value | Behaviour |
| --- | --- |
| `enabled` (default; also when unset or empty) | Unauthenticated stream features carry `<authentication xmlns='urn:xmpp:sasl:2'/>` with SCRAM-SHA-256 and OAUTHBEARER beside the SASL1 `<mechanisms/>`. |
| `disabled` | SASL2 is not advertised. A SASL2 `<authenticate/>`, `<response/>` or `<abort/>` is answered with a SASL2 `<failure/>` carrying `<invalid-mechanism/>`. SASL1 is unchanged. |

Any other value fails startup with an `invalid WADDLE_XMPP_SASL2` error, and
the chart refuses to render it. The selection is read once at startup;
changing it takes a rollout. SASL1 stays available in both modes, so clients
that do not implement SASL2 are unaffected.

## Inline features

Where startup selected the durable stream-management authority (every
production start), the SASL2 feature advertises an `<inline/>` offer with:

- `<sm xmlns='urn:xmpp:sm:3'/>`: XEP-0198 resumption inside `<authenticate/>`.
- `<bind xmlns='urn:xmpp:bind:0'>` listing `urn:xmpp:sm:3`: XEP-0386 Bind 2,
  with stream management enabled inside the bind.

The server processes exactly what it advertises. A state without the durable
authority advertises no `<inline/>` and refuses inline requests as a
malformed request. A successful inline resume reports `<resumed/>` inside
`<success/>` and skips the bind; a failed resume reports `<failed/>` and
falls back to the Bind 2 request, if present.

## Clients

The shared client runtime (`waddle-xmpp-client`, used by the web client
through WASM) negotiates SASL2 only on a `wss://` endpoint that offers
OAUTHBEARER over SASL2, and sends inline requests only when the server
advertises them. Over plain `ws://`, as in local development and the
loopback test fixtures, it keeps using SASL1.

## Rolling back

Set `xmpp.sasl2: disabled` in the HelmRelease and let Flux roll the
deployment. Clients reconnect with SASL1.

## Verification

The suites below run with the runtime selection, not a build feature:

```sh
cargo nextest run --profile ci -p waddle-server --all-features \
  -E 'binary(sasl2_dark_ws) | binary(sasl2_inline_dark_ws) | binary(sm_durable_inline) | binary(server_capability_manifest)'
```

`sasl2_dark_ws` covers both selections, including the default. These
loopback tests prove the application handshake only; TLS termination is the
gateway's and is not exercised.
