# Startup ownership and environment safety

Cleanup plan for #2020 and #2024:

1. Establish the existing listener and VAPID lifecycle behavior with their
   focused suites before editing.
2. Remove the unused `restart` / `RestartError` API. Its process-wide descriptor
   remapping can invalidate descriptors owned by runtime threads; the server
   never invokes it, so deletion preserves the running server's behavior.
3. Capture inherited descriptors once in the synchronous main prologue, before
   runtime/telemetry threads or other descriptor owners exist. State the
   exclusive ownership requirement explicitly at the unsafe capture boundary.
   Convert owned standard listeners into Tokio listeners after entering Tokio.
4. Retain environment values instead of mutating the environment after threads
   start. Preserve VAPID parse, signer-initialization and persistence failures.
   Update the documented secret exposure policy and existing lifecycle coverage.
5. Format, run the focused suites, and check the unsafe-code lint. Coordinate
   compilation through the shared target cache before workspace validation.

## Environment and secret exposure policy

Operator instructions for `WADDLE_VAPID_PRIVATE_KEY`, including the required
removal from the deployment after the first successful boot, are in the
[VAPID bootstrap runbook](../../server/docs/operations/vapid-bootstrap.md).

`LISTEN_FDS` and `LISTEN_FD_NAMES` remain in the process environment. They
describe descriptors transferred exclusively by the launcher at exec. Capture
is performed only once before runtime startup; descriptors are marked
close-on-exec after adoption so a future unconfigured child cannot inherit
them.

## Restart behavior

SIGTERM and SIGQUIT both cause the running server to stop accepting and drain.
SIGQUIT records restart intent, but the server does not spawn or re-exec a new
process. Starting a replacement remains the supervisor/operator's responsibility;
there is no promise of automatic zero-gap restarts.
