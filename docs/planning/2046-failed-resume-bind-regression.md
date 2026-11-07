# Failed-resume bind regression

After a resumed binding is published, a replay-gap failure can leave an
accepted outbound envelope or outstanding reservation on the old endpoint.
The rollback refuses to hand that endpoint to a fresh bind, but the resource
binding handler may already have created a successful bind result.

## Plan

1. Exercise the production frame and registration sequence with a deterministic
   barrier that retains old endpoint work through resume finalization.
2. Assert that an unresolved endpoint never produces a successful fresh-bind
   response or exposes a routable resource. Preserve the existing empty-queue
   path and assert that old work remains owned until disposition is proven.
3. Keep the regression red in this draft until a separate reviewed serving fix
   establishes safe custody and retry behavior. Do not change serving behavior
   or enable provisional routing in this slice.

Use native WebSocket coverage if the existing seam can establish acceptance and
ordering. If it cannot, use a deterministic lower-level production-path test and
state the missing transport evidence. Record exactly which tests run; a local
test is not proof of cross-node recovery or durable custody.
