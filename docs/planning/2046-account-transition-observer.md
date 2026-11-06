# Dark account-resource transition observation for #2046

## Scope

Observe the existing `UserActor` resource registration, replacement and removal decisions with the typed P3.2 transition model. The actor and current endpoint/socket paths remain the only production writers. The observer is behind test support, produces comparison evidence only, and cannot gate routing, registration, XMPP replies or cleanup. It must not maintain a second mutable serving snapshot.

## Plan

1. Map each observed actor operation to the existing exact account claim, physical `ConnectionRef`, binding attempt and resource identity. Do not treat `LocalConnectionBindingId` as a durable connection or account claim.
2. Feed committed actor outcomes into a private observer adapter and compare the resulting model snapshot/receipts with the actor state. Begin with register, exact replacement, stale removal and retirement; report unmodeled account fields explicitly.
3. Add deterministic tests for publication failure, late cleanup after successor bind, duplicate/stale operations and actor ambiguity. Verify successful resume preserves logical resource state and failed resume followed by fresh bind gets a new physical binding where the fixture can establish that boundary.
4. Run focused account/endpoint/SM tests, formatting and scoped Clippy, then independent final review. Keep the draft open until the observed contracts pass.

## Activation gates

Production authority requires one selected endpoint writer, real P3.1 physical connection ownership, durable P3.3 session/frontier authority, P2 acceptance custody, cross-node claim/order proof and retirement of the DashMap-first dual registration path. Account-wide activity, pending subscriptions, presence ordering and remote revision handling need explicit ownership before any model result becomes authoritative.
