# P3.1 dark actor-driver foundation

Status: implemented in draft PR #2205, rebased onto hosted main
`1f316ee9ca9615ed0b7df50a7b6c5ae9bb3cc373`. The initial plan commit was
created from `7057aac89fc188fa1898470ac3f0f880702485de`. This slice keeps the
connection actor unselected by `/ws` and does not close #2137 or #2046.

## Current gap

PR #2203 added a production-compiled `ActorLocalEndpointOwner` and retained
outbound custody. Its direct writer explicitly refuses `PeerStanza`, because
the actor path does not yet run the existing recipient pass. The live WebSocket
path already has shared handlers for XMPP frame processing and recipient
interpretation (`handle_xmpp_frame_with_admission` and
`drive_interpret_loop`). `ConnectionActorCore` owns `WsConnState`; this slice
adds the driver-facing methods for composing those handlers while leaving the
live path unchanged.

## Bounded implementation

The production-compiled, unselected driver surface on `ConnectionActorCore`
can:

- process one inbound XMPP frame through the existing admission-aware frame
  handler and return its existing typed `ResponseBatch`;
- dispatch one retained peer stanza through the existing state-machine and
  recipient interpreter, returning the existing `DriveOutcome` for a future
  transport adapter to settle. Both values are wrapped in
  `ConnectionActorDriverCompletion`.

Keep the `WsConnState` private to the owner. The peer envelope is borrowed so
its actor custody record remains intact. The adapter surface exposes the
owner's existing `ConnectionRef` by borrow and can request stop; it creates no
new identity. This slice does not write frames, acknowledge callbacks, publish
a binding, activate an endpoint, or claim recovery custody. It does not add
another routing implementation. The core, constructor, and driver methods are
visible within the WebSocket module so a future sibling adapter can construct
and drive it; `/ws` has no call site.

If authority is revoked or stop arrives while a handler is running, the driver
returns `CompletionUnknown` with the handler result retained. Callers must keep
that result and custody represented; they must not treat the condition as a
pre-handler rejection and replay the input. `CurrentAtCompletion` is only a
snapshot; a transport adapter must revalidate before emitting. Dropping the
driver future while the handler is awaited also drops the handler future, so
this slice does not provide cancellation-safe custody. An actor-owned task or
another retained-completion mechanism remains a blocking activation gate.

Focused tests exercise stream-open frame processing and peer-recipient
interpretation while asserting that the original retained envelope remains
unchanged.

## Boundaries and remaining gates

Leave `/ws`, `connection.rs`, registry publication, actor endpoint queues,
feature advertisements, workflow files, and GitHub Actions unchanged. The
returned frames are not evidence of transport acceptance. The actor still
needs its mailbox/control scheduling, physical endpoint integration, exact
account-resource/endpoint/node/SM tuple, pending and ordered custody, remote
acceptance, resume/detach lifecycle, and retirement of the legacy loop before
cutover. XEP wire behavior is unchanged, so this slice adds no XEP behavior.

## Local verification

- Focused actor-owner nextest run on `1f316ee9`: 4 passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `cargo check -p waddle-server --lib --locked`: passed on `1f316ee9`.
- The same check with `--features clustering`: passed on `1f316ee9`.
