# Dark account-resource transition observation for #2046

## Scope

Record bounded, inert evidence of existing `UserActor` resource registration, replacement and removal decisions. Compare with the typed P3.2 transition model only when a disposable fixture independently witnesses every required claim, exact endpoint publication, binding and lifecycle receipt. Otherwise return an explicit inconclusive or unsupported result. The actor and current endpoint/socket paths remain the only production writers. The observer is behind test support and cannot gate routing, registration, XMPP replies or cleanup. It must not maintain a second mutable serving snapshot or retain actor/entry/channel handles.

## Plan

1. Capture minimal copied IDs, values and the actual child mutation outcome synchronously under `test-utils`, without awaits or changes to actor replies. Label a fixture's actor lifetime with one stable **synthetic** `UserClaimIdentity`; it is not durable claim authority. Remote mirrors without `BindingRef` are unmodeled. Never infer publication from actor registration or removal from a tell.
2. Feed only fully witnessed local operations into a private adapter. Direct actor replacement into an occupied slot must be reported as model divergence or unsupported until the required retirement facts exist. Do not invent endpoint, custody, teardown, presentation or resume receipts. Compare only fixture-controlled fields; shared atomics and separate presence payloads are not a coherent actor snapshot.
3. Test local register/replacement/stale removal, absent and unconditional removal, publication failure, timed-out child commit, remote entry without `BindingRef`, observer exhaustion and closed-channel/conflict-close coverage. Missing evidence or journal saturation must be inconclusive and leave actor behavior unchanged. Resume observations require a separately witnessed SM finalization; failed resume then fresh bind on the same socket keeps `ConnectionRef` but gets a new `BindingRef`.
4. Run focused account/endpoint/SM tests, formatting and scoped Clippy, then independent final review. Keep the draft open until the observed contracts pass.

## Activation gates

Production authority requires one selected endpoint writer, real P3.1 physical connection ownership, durable P3.3 session/frontier authority, P2 acceptance custody, cross-node claim/order proof and retirement of the DashMap-first dual registration path. An exact account claim/actor incarnation, global publication/retirement receipts, account-wide activity, pending subscriptions, presence ordering and remote revision handling need explicit ownership before any model result becomes authoritative. An actor decision observation alone proves none of those gates.
