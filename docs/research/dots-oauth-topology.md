# OAuth 2.1 topology for ChatGPT plugin auth against Waddle

Research for [#1915](https://github.com/waddle-social/waddle/issues/1915), part of map [#1914](https://github.com/waddle-social/waddle/issues/1914).
Sources were fetched on 2026-10-05. Code references are to `main` at `d17e4b61b`.

## Recommendation

**Option (a).** Waddle should run its own small OAuth 2.1 authorization server (AS) for agent connectors. It issues opaque, audience-bound tokens tied to one owner and one personal-agent grant, i.e. `(user, plugin)`.

It does not authenticate users itself. The consent page signs the user in through Waddle's existing login: any configured OIDC/OAuth2 provider, or a native account.

Option (b), pointing ChatGPT straight at the upstream IdP, is rejected for three reasons:

- **It fails against prod's IdP today.** Prod's upstream IdP is Colony. Colony rejects ChatGPT's token request, has no CIMD support, and cannot mint tokens for a Waddle `resource`. Details are in the option (b) section below.
- **It can't serve every Waddle deployment or user.** It assumes the upstream IdP is a full, current OAuth 2.1 server. Waddle supports arbitrary providers (GitHub-style plain OAuth2, Google, and others) and native SCRAM accounts that have no upstream identity at all.
- **It can't express the binding.** Even a fully fixed Colony can only bind a token to `(Colony user, DCR client)`. It cannot bind to Waddle's personal-agent grant. Enabling, consent and disabling would all have to happen out of band from the OAuth flow.

Option (a) makes the OAuth consent screen *be* the "enable my dot" step. It also makes "disable" an immediate, in-transaction revocation.

## 1. What ChatGPT requires (exact spec requirements)

Primary sources:

- **OA**: OpenAI, *Authentication*: <https://developers.openai.com/plugins/build/auth>, plus its twin <https://developers.openai.com/plugins/build/auth.md>. The two had identical text on 2026-10-05. `apps-sdk/build/auth` now redirects here.
- **MCP**: the MCP authorization spec, latest dated version 2026-07-28: <https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization>. OA names 2025-11-25 as its conformance target, but cites 2026-07-28 for issuer validation.

| # | Requirement | Source |
|---|---|---|
| R1 | **Protected resource metadata (PRM).** The MCP server serves `/.well-known/oauth-protected-resource` (RFC 9728). On a 401 it sends `WWW-Authenticate: Bearer resource_metadata="…"`. PRM fields: `resource` is the canonical HTTPS id, which "ChatGPT sends … as the `resource` query parameter". `authorization_servers` holds issuer URLs; MCP says it **MUST** contain at least one. `scopes_supported` is optional. | OA §"Host protected resource metadata on your MCP server". MCP [authorization-server-discovery](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/authorization-server-discovery) §"Authorization Server Location". RFC 9728 §2, §3.3 (`resource` must exactly equal the id the well-known path was built from). |
| R2 | **AS metadata.** Either RFC 8414 `/.well-known/oauth-authorization-server` or OIDC discovery; clients must support both. `issuer` must equal the PRM `authorization_servers` entry exactly. Path issuers use RFC 8414 path insertion. | OA §"Publish OAuth metadata from your authorization server". MCP authorization-server-discovery §"Authorization Server Metadata Discovery". |
| R3 | **PKCE S256 is mandatory.** "`code_challenge_methods_supported`: must include `S256`. MCP servers are unsupported when their authorization server metadata omits this field." | OA §"Publish OAuth metadata…" and §"Support the authorization-code flow". MCP [security-considerations](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/security-considerations) §"Authorization Code Protection". |
| R4 | **Client registration.** ChatGPT supports CIMD, DCR, and predefined clients, and "prioritizes CIMD when it is available". Its CIMD `client_id` is `https://chatgpt.com/oauth/client.json`, or `https://chatgpt.com/oauth/{callback_id}/client.json` when the AS lacks issuer identification. CIMD token auth is `none` or `private_key_jwt`; the JWKS is at `https://chatgpt.com/oauth/jwks.json`. MCP 2026-07-28 says "Dynamic Client Registration is deprecated. New implementations should use Client ID Metadata Documents instead." The AS advertises CIMD with `client_id_metadata_document_supported: true`. | OA §"OAuth flow" step 2 and §"Client registration". MCP [client-registration](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/client-registration). [draft-ietf-oauth-client-id-metadata-document-02](https://www.ietf.org/archive/id/draft-ietf-oauth-client-id-metadata-document-02.txt) §6. |
| R5 | **ChatGPT's live client metadata** (`GET https://chatgpt.com/oauth/client.json`, 2026-10-05): `redirect_uris: ["https://chatgpt.com/connector_platform_oauth_redirect"]`, `grant_types: ["authorization_code","refresh_token"]`, `token_endpoint_auth_method: "private_key_jwt"` (RS256), and `token_endpoint_auth_methods_supported: ["none","private_key_jwt"]`. | Fetched directly. |
| R6 | **Redirect URI and issuer identification (RFC 9207).** If the AS sets `authorization_response_iss_parameter_supported: true` and returns `iss` on *every* authorization response, success and error, ChatGPT uses the stable redirect `https://chatgpt.com/connector_platform_oauth_redirect`. Otherwise it uses `https://chatgpt.com/connector/oauth/{callback_id}`, which must be allowlisted per connection. `iss` is compared exactly. The AS **MUST** match redirect URIs exactly. | OA §"Protect callbacks with issuer identification" and §"Redirect URL". MCP security-considerations §"Open Redirection". MCP §"Authorization Response Validation". |
| R7 | **Resource indicators and audience.** "Expect ChatGPT to append `resource=…` to both the authorization and token requests. Configure your authorization server to copy that value into the access token (commonly the `aud` claim)". The MCP server **MUST** validate audience and **MUST NOT** accept or pass through other tokens. | OA §"Echo the `resource` parameter throughout the OAuth flow". MCP §"Resource Parameter Implementation" and §"Token Handling". RFC 8707 §2. |
| R8 | **No machine-to-machine grants.** "ChatGPT does **not** support machine-to-machine OAuth grants such as client credentials, service accounts, or JWT bearer assertions, nor can it present custom API keys or customer-provided mTLS certificates." Only the authorization-code flow with a human user is available. | OA §"Client identification". |
| R9 | **mTLS is optional and identifies the transport origin, not the user.** "ChatGPT now presents an OpenAI-managed client certificate when establishing TLS connections to MCP servers." The leaf SAN `dnsName` is `mtls.prod.connectors.openai.com`, chained to OpenAI's published CAs. Don't pin the leaf. "Use mTLS to authenticate ChatGPT as the MCP client. Continue to use OAuth 2.1 to authenticate the end user". This is not RFC 8705 token-endpoint client auth. | OA §"Mutual TLS (mTLS)". |
| R10 | **Per-tool `securitySchemes`.** Each tool declares `noauth` and/or `oauth2` with scopes; `_meta.securitySchemes` is a back-compat mirror. The linking UI appears only when the tool declares `oauth2` *and* a runtime tool error carries `_meta["mcp/www_authenticate"]`, whose value includes `error` and `error_description`. | OA §"Triggering authentication UI". [Plugins reference](https://developers.openai.com/plugins/reference). |
| R11 | **Scopes.** The client uses the challenge `scope`, otherwise PRM `scopes_supported`. Insufficient scope is a 403 with `error="insufficient_scope"`, and step-up requests the union of scopes. If the AS advertises OIDC scopes, ChatGPT requests them by default. | MCP §"Scope Selection Strategy". OA §"OIDC scopes". |
| R12 | **Refresh.** ChatGPT registers `refresh_token`. Clients **MUST NOT** assume a refresh token will be issued. ASes **MUST** rotate refresh tokens for public clients and **SHOULD** issue short-lived access tokens. MCP servers **SHOULD NOT** advertise `offline_access` in PRM. | R5. MCP §"Refresh Tokens". MCP security-considerations §"Token Theft". |
| R13 | **Revocation.** Neither OpenAI nor MCP documents ChatGPT calling an RFC 7009 revocation endpoint on disconnect. What is documented: a rejected token gets a `401` plus `WWW-Authenticate`, which "tells the client to run the OAuth flow again", and servers should "plan for token revocation". | OA §"Implementing token verification" and §"Testing and rollout". [Troubleshooting](https://developers.openai.com/plugins/deploy/troubleshooting). |
| R14 | **Stable profile id (multi-account).** An optional profile tool returns an opaque id. It must stay the same "across token refresh, reconnect, and scope upgrades" and must "never be reassigned to a different profile after deletion". Emails and names must not be used. | OA §"Define a stable profile identity". |
| R15 | **Confused deputy.** An AS that proxies login to an upstream IdP with a static client id **MUST** get user consent for each client *before* forwarding upstream. It should keep a per-user registry of approved client ids. With CIMD, the AS may allowlist trusted client-id domains. | MCP security-considerations §"Confused Deputy Problem". [MCP security best practices](https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices) §"Confused Deputy Problem" and §"CIMD Trust Policies". |
| R16 | **Submission, only if published.** To support workspace domain restrictions, an OAuth plugin exposes UserInfo with `email` and `email_verified: true`. Reviewers need a full demo login with no inaccessible 2FA. | OA §"Support workspace domain restrictions". [Plugin guidelines](https://developers.openai.com/plugins/plugin-guidelines). |

OpenAI's counter-advice: "We _strongly_ recommend that you use an existing established identity provider rather than implementing authentication from scratch yourself" (OA §"Choosing an identity provider"). Option (a) follows the spirit of this. Waddle's AS does no primary authentication; it delegates that to the established IdP or native credentials and only issues delegation tokens.

## 2. Waddle today

- **Upstream login is a multi-provider broker.** `AuthProviderConfig` supports `oidc` and `oauth2` kinds, plus optional upstream DCR and DPoP (`server/crates/waddle-server/src/auth/providers.rs:25-59`). `start_authorization` adds PKCE S256, state and nonce, and performs upstream DCR when it is configured (`server/crates/waddle-server/src/server/routes/auth/state.rs:168-269`).
- **Prod uses exactly one provider: Colony**, configured as `issuer: https://colony.waddle.social`, `dynamic_client_registration: true`, `require_dpop: true` (`infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml:188-189`). The chart's own example provider is GitHub plain OAuth2 (`server/charts/waddle-server/values.yaml:258-261`).
- **Upstream subjects map to Waddle users by `(issuer, subject)`** through `auth_identities` (`server/crates/waddle-server/src/auth/identity.rs:112-123`). Waddle keeps no upstream tokens. The callback creates a Waddle `Session`, an opaque UUID that is stored hashed and valid for 30 days (`server/crates/waddle-server/src/server/routes/auth/callback.rs:212-307`; `server/crates/waddle-server/src/auth/session.rs:42-83`, `:152`).
- **Native accounts** (XEP-0077 + SCRAM, no upstream IdP) also exist (`server/crates/waddle-server/src/auth/native.rs:1-11`).
- **There is already a first-party OAuth façade for XMPP clients (XEP-0493).** It consists of:
  - `/.well-known/oauth-authorization-server` at the server root, advertising `scopes_supported: ["xmpp"]` and `authorization_code` only;
  - `/api/auth/xmpp/authorize` and `/api/auth/xmpp/token`, where the token endpoint returns the Waddle session id as the bearer (`server/crates/waddle-server/src/server/routes/xmpp_oauth.rs:19-56`, `:314-323`).

  The root metadata URL is advertised to XMPP clients (`server/crates/waddle-server/src/server/xmpp_auth_state.rs:67-74`). So **the root RFC 8414 document is taken.** A dots AS needs its own issuer.
- **SASL OAUTHBEARER** resolves a bearer through `SessionManager::validate_session` to `(user_jid, localpart)` (`server/crates/waddle-server/src/server/xmpp_auth_state.rs:38-65`). A connector token must never be accepted here. It must not become a full user session, because of the MCP no-passthrough rule (R7).
- **Extension authority (RFC 0018 §3.1).** `IngressPrincipal::Extension(ExtensionPrincipal { grant, requester, sender })` asserts an active `extension_grants` row that matches the plugin and scope, and asserts that the requester account still exists, held `FOR SHARE` through commit (`server/docs/rfcs/0018-ingress-authority-cutover.md:200-214`; `server/crates/waddle-server/src/ingress/principal.rs:5-18`, `:72-94`). §3.1 also states: "Grant revocation is configuration-driven only … No runtime unload or grant revocation API exists" (`0018…md:211-214`).
- **The grants table is per-plugin, not per-owner.** Its columns are `grant_id, plugin_id, scope, room_jid, granted_at, revoked_at`, with unique active `Send` per `plugin_id` (`server/crates/waddle-server/src/db/migrations/waddle.rs:983-1008`). `sync_configured` revokes every active grant that is not in the configured set (`server/crates/waddle-server/src/ingress_uow/extension_grants.rs:50-99`, revoke at `:86-87`).

## 3. Option (b): delegate to the upstream IdP. Facts against prod's Colony

Colony is a Better Auth (`@better-auth/oauth-provider` 1.6.7) OIDC provider on Cloudflare Workers (`colony/README.md`, `colony/src/lib/auth.ts:38-59`). Its live metadata (`GET https://colony.waddle.social/.well-known/oauth-authorization-server`, 2026-10-05) advertises:

- S256 PKCE;
- `authorization_response_iss_parameter_supported: true`;
- a `registration_endpoint` (DCR, unauthenticated: `auth.ts:48-49`);
- `refresh_token` and `offline_access`;
- a `revocation_endpoint`;
- `client_credentials` in `grant_types_supported`.

Against R1–R16, Colony as deployed fails in three places:

1. **The token endpoint rejects ChatGPT.** Every `/oauth2/token` request without a valid `DPoP` header gets `400 invalid_dpop_proof` (`colony/src/pages/api/auth/[...all].ts:120-137`). ChatGPT's client metadata (R5) and OA never mention DPoP.
2. **ChatGPT's `resource` is refused.** Better Auth's `checkResource` returns `400 invalid_request "requested resource invalid"` for any `resource` outside `validAudiences`, which defaults to Colony's own base URL. Colony does not configure it (`colony/node_modules/@better-auth/oauth-provider/dist/index.mjs:456-467`). ChatGPT always sends `resource` (R7).
3. **No CIMD support.** Neither the metadata nor the library has `client_id_metadata_document_supported` (grep of the 1.6.7 dist finds none). ChatGPT would fall back to DCR, which MCP has deprecated (R4).

All three are fixable in Colony: exempt registered public clients from the DPoP gate, set `validAudiences`, and accept DCR. Even then, structural problems remain:

- **No `(user, plugin)` binding.** A Colony token's subject is the Colony user and its `client_id` is a DCR client. Colony has no concept of a Waddle personal agent, its handle, or its grant. So the binding would live only on the Waddle side, keyed on Colony `sub` plus `extension_grants`. The consent the user gave Colony ("let ChatGPT see my profile") is not the consent Waddle needs ("let my dot act as `@handle` in rooms I invite it to").
- **Enable and disable fall outside the OAuth flow.** The user must enable the agent in Waddle *before* connecting in ChatGPT, and the flow cannot create the agent.

  On disable, Waddle can refuse calls by checking the grant, but Colony keeps refreshing the tokens it issued (30-day default refresh lifetime, `index.mjs:2728-2730`). The only exit is a 401 loop where ChatGPT re-authenticates successfully at Colony and is then refused again by Waddle.
- **Not portable.** It works only where the upstream is a full OAuth 2.1 AS with CIMD/DCR, RFC 8707 and RFC 9207. GitHub-style OAuth2 providers, Waddle's chart default, have none of this. Native SCRAM users have no upstream identity at all (`native.rs:1-11`; RFC 0018 §3.1 explicitly covers `native_users` as requesters).
- **Cross-system coupling.** A dot change would require coordinated changes in a separately deployed Workers/D1 app.
- **Consent surface.** The consent screen would be Colony's generic one, not one naming the agent and its owner. Which clients may obtain the Waddle MCP audience would depend on Colony's client-registration policy rather than Waddle's.

## 4. Option (a): Waddle-run AS, specified against R1–R16

**Topology**

- Resource: `https://<base>/mcp`. PRM is served at `/.well-known/oauth-protected-resource/mcp` (path-inserted) and also referenced from every 401 (R1).
- Issuer: a path issuer such as `https://<base>/oauth/agents`, with RFC 8414 metadata at `/.well-known/oauth-authorization-server/oauth/agents`. This keeps it separate from the XEP-0493 root document (R2, §2).
- Metadata advertises:
  - `code_challenge_methods_supported: ["S256"]` (R3);
  - `client_id_metadata_document_supported: true`;
  - `token_endpoint_auth_methods_supported: ["none", "private_key_jwt"]` (R4);
  - `authorization_response_iss_parameter_supported: true` (R6);
  - `grant_types_supported: ["authorization_code", "refresh_token"]` with no `client_credentials` (R8);
  - no `registration_endpoint`. DCR is not needed when CIMD is offered (R4).

**Client trust**

- Accept CIMD `client_id`s only from an operator allowlist, defaulting to `https://chatgpt.com/oauth/client.json` (R15 "CIMD Trust Policies").
- Fetch the document and check that `client_id` equals its URL exactly. Match `redirect_uri` exactly against its `redirect_uris` (R4, R6).
- If `private_key_jwt` is used, verify against the JWKS that `jwks_uri` points to.

**Authorize**

1. Require `resource` to equal the MCP resource exactly; otherwise return `invalid_target` (R7).
2. Sign the user in through existing machinery: a new `PendingFlow` variant beside `Browser | Device | Xmpp` (`routes/auth.rs:81-95`), or the existing web session cookie, or native login.
3. **Show Waddle's consent page before forwarding to any upstream IdP.** This satisfies R15 even though upstream uses a static or DCR client id. The page names the client (ChatGPT) and the personal agent (`@handle-dot`), and lets the user pick or confirm the handle. Approval atomically creates or reactivates the owner's personal agent JID and its personal grant (map decision).
4. Return `code`, `state` and `iss` on every response, including errors (R6).

**Token**

- **Access token:** opaque and short-lived (recommend 1 h). Store it hashed, using the same keyed-hash approach as `SessionManager::token_hash` (`session.rs:152`). Its row binds:
  - `grant_id` (the personal grant);
  - `owner_jid`;
  - `agent_jid`;
  - `client_id`;
  - `resource`;
  - `scopes`;
  - `expires_at`.
- **Refresh token:** rotated on every use, with reuse detection that kills the token family (R12). Recommend a sliding 30-day lifetime, capped by grant liveness. Issue it only when the grant is active. Do not advertise `offline_access` in PRM.
- **Format:** opaque rather than JWT. Every call already needs a database check of the grant (below), so a JWT buys nothing and makes revocation harder.

**Validation at `/mcp`**

1. Look up the token hash and check it is unexpired, that `resource == /mcp`, and its scopes. On failure return `401` + `WWW-Authenticate: Bearer resource_metadata=…`, and in tool results `_meta["mcp/www_authenticate"]` with `error` and `error_description` (R1, R10, R13).
2. Build `ExtensionPrincipal { grant: <personal grant ref>, requester: Some(owner_jid), sender: agent_jid }`.

   Admission already re-asserts the grant and the owner account `FOR SHARE` inside the ingress transaction (`principal.rs:72-94`). So a disabled agent or deleted owner fails as `principal_missing` even for a token that is still valid.

   The token itself is never a Waddle session and is never accepted by SASL OAUTHBEARER (R7).
3. Optional: verify the mTLS leaf SAN `mtls.prod.connectors.openai.com` at the gateway as defense in depth (R9). This needs TLS pass-through or client-cert forwarding at the Cilium gateway, and is not required.

**Disable and revocation**

- Disabling the agent sets the personal grant's `revoked_at` and deletes that grant's access and refresh tokens in one transaction.
- In-flight admissions are already serialized against it by the `FOR SHARE` assertion.
- ChatGPT's next call gets a 401 and its refresh gets `invalid_grant`. The user sees the re-link UI, and re-linking runs the consent page again, which is the re-enable path (R13).
- Optionally expose an RFC 7009 `revocation_endpoint` for completeness. ChatGPT is not documented to call it.

**Profile id (R14):** return an opaque per-grant-lineage UUID that persists across disable and re-enable. Do not use the JID, because a JID embeds the user-chosen handle.

**Scopes:** a minimal set matched to the tool families, e.g. `agent:read` (occupied-room reads) and `agent:post` (DM the owner, reply in rooms). Declare them per tool in `securitySchemes` (R10, R11). Do not advertise OIDC scopes; the AS is plain OAuth, so ChatGPT will not default-request `openid email profile`.

**R16** applies only if the plugin is published to the directory. A UserInfo endpoint with a verified email is an open question for native accounts (see below).

### Changes this implies for the extension-grant model

These are inputs for the decision tickets, not decided here.

- **Ownership and uniqueness.** `extension_grants` needs an owner (and agent JID) dimension for personal grants. Personal `Send` uniqueness becomes `(plugin_id, owner_jid)`. Today the unique index is on `(plugin_id)` only (`waddle.rs:993`, `:1007`).
- **Sync must skip personal grants.** `sync_configured` must leave personal-tier rows alone; today it revokes every active row missing from operator config (`extension_grants.rs:86-87`).
- **Runtime revocation.** RFC 0018 §3.1's "configuration-driven only … no runtime … revocation API" needs a personal-tier exception: a runtime revoke on disable.
- **Effective sender.** The effective sender for a personal agent is the per-owner agent JID, not the shared plugin actor JID that §3.1 uses for groupchat (`0018…md:216-217`).

## 5. Trade-offs

| | (a) Waddle AS | (b) Upstream IdP (Colony) |
|---|---|---|
| Works with ChatGPT today | Once built | No: DPoP gate, `resource` refused, no CIMD |
| `(user, plugin)` binding in the token | Yes (token → `grant_id`) | No: `(Colony sub, DCR client)` only; binding lives outside the token |
| Consent names the agent and creates it | Yes, consent = enable | No: Colony's generic consent; enable happens out of band |
| Disable → immediate cutoff | Grant revoke deletes tokens; next call 401 | Calls refused, but Colony keeps refreshing; 401 loop |
| Native / GitHub-OAuth2 / self-hosted deployments | Works (login is delegated) | Excluded |
| Build cost | New endpoints, CIMD fetcher, token store, consent UI | Changes to a separate Workers app, plus the same Waddle-side grant checks anyway |
| "Use an established IdP" (OA) | Primary authentication still delegated; only delegation tokens issued | Directly aligned |
| XMPP-native rule | OAuth/MCP HTTP is the sanctioned connector surface (map ADR amendment); tool semantics still map onto XMPP ingress | Same |

The deciding asymmetry: (b) still needs every Waddle-side grant check that (a) needs, plus Colony changes. It still can't bind or revoke at the token level, and it excludes every non-Colony deployment and every native user.

## 6. Open questions and limits

- **Native accounts without verified email.** R16 (UserInfo `email_verified`) applies only if the dot plugin is *published*. The research did not establish whether a personal dot connector must be a published directory plugin or can be a developer-mode/private connector.
- **Unverified ChatGPT behaviour.** No documentation was found on whether ChatGPT calls a revocation endpoint on disconnect, requests `offline_access`, or expects particular token lifetimes. The lifetimes above are recommendations, not requirements.
- **mTLS at the gateway.** Verifying the client certificate depends on TLS termination at the Cilium gateway, which was not investigated.
- **Colony limits.** The Colony findings are for `@better-auth/oauth-provider` 1.6.7 as vendored in `colony/node_modules`. A newer Better Auth may add CIMD; that would not change the structural points in §3.
