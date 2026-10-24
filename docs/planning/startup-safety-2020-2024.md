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

`WADDLE_VAPID_PRIVATE_KEY` is an optional bootstrap input. The process retains
the variable after successful persistence as well as on failure. The running
process and principals that can inspect its environment remain inside the
secret's trust boundary; clearing the variable never erased the original OS
environment or other copies reliably. Decoded temporary key material remains
zeroized, and persisted private key material remains sealed at rest.

There is no production child-process or exec path after removing the unused
restart API. Future child launchers must construct an explicit environment
that excludes `WADDLE_VAPID_PRIVATE_KEY`, `LISTEN_FDS` and `LISTEN_FD_NAMES`
(for example with `Command::env_remove`), unless implementing a separately
reviewed listener handoff. The parent environment must not be mutated to
configure children. Operators can remove the bootstrap variable from subsequent
deployment configuration after successful provisioning to load the stored key.

`LISTEN_FDS` and `LISTEN_FD_NAMES` also remain in the process environment.
They describe descriptors transferred exclusively by the launcher at exec.
Capture is performed only once before runtime startup; descriptors are marked
close-on-exec after adoption so a future unconfigured child cannot inherit them.

## Restart behavior

SIGTERM and SIGQUIT both cause the running server to stop accepting and drain.
SIGQUIT records restart intent, but the server does not spawn or re-exec a new
process. Starting a replacement remains the supervisor/operator's responsibility;
there is no promise of automatic zero-gap restarts.
