# P3.1 dark actor-driver foundation

Status: scoped plan on hosted main `7057aac89fc188fa1898470ac3f0f880702485de`.
This slice keeps the connection actor unselected by `/ws` and does not close
#2137 or #2046.

## Current gap

PR #2203 added a production-compiled `ActorLocalEndpointOwner` and retained
outbound custody. Its direct writer explicitly refuses `PeerStanza`, because
the actor path does not yet run the existing recipient pass. The live WebSocket
path already has shared handlers for XMPP frame processing and recipient
interpretation (`handle_xmpp_frame_with_admission` and
`drive_interpret_loop`). The production `ConnectionActorCore` owns
`WsConnState`, but has no driver-facing methods for composing those handlers.

## Bounded implementation

Add a production-compiled, unselected driver surface on `ConnectionActorCore`
that can:

- process one inbound XMPP frame through the existing admission-aware frame
  handler and return its existing typed `ResponseBatch`;
- dispatch one retained peer stanza through the existing state-machine and
  recipient interpreter, returning the existing `DriveOutcome` for a future
  transport adapter to settle.

Keep the `WsConnState` private to the owner. The peer envelope is borrowed so
its actor custody record remains intact. This slice does not write frames,
acknowledge callbacks, publish a binding, activate an endpoint, or claim
recovery custody. It does not add another routing implementation.

## Boundaries and remaining gates

Leave `/ws`, `connection.rs`, registry publication, actor endpoint queues,
feature advertisements, workflow files, and GitHub Actions unchanged. The
returned frames are not evidence of transport acceptance. The actor still
needs its mailbox/control scheduling, physical endpoint integration, exact
account-resource/endpoint/node/SM tuple, pending and ordered custody, remote
acceptance, resume/detach lifecycle, and retirement of the legacy loop before
cutover. XEP wire behavior is unchanged, so this slice adds no XEP behavior.
