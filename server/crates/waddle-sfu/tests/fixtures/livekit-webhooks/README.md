# LiveKit webhook ProtoJSON fixtures

These synthetic call events were serialized by the real upstream
`github.com/livekit/protocol/livekit` v1.45.3 Go protobuf types using
`google.golang.org/protobuf/encoding/protojson.MarshalOptions{Indent: "  "}`.
LiveKit server v1.11.0 uses that protocol version; the JSON is not hand-written
and contains no production identities or credentials.

Each event has `CreatedAt: 1782907200`, room `general@muc.test` / `RM_protojson`,
and (except `room_finished`) participant `alice@waddle.test/desktop` /
`PA_protojson`. The room and participant deliberately include additional real
upstream fields so the tests exercise Waddle's typed projection of a full event.
ProtoJSON emits both `createdAt` and the other int64 fields as decimal strings.

Sources:

- [LiveKit v1.11.0 dependencies](https://github.com/livekit/livekit/blob/v1.11.0/go.mod)
- [WebhookEvent schema](https://github.com/livekit/protocol/blob/v1.45.3/protobufs/livekit_webhook.proto)
- [Canonical ProtoJSON integer representation](https://protobuf.dev/programming-guides/json/#representation-of-each-type)

To regenerate, construct the four `livekit.WebhookEvent` kinds with those fixed
values and marshal them through `protojson`; no Go dependency is added to the
Waddle Rust workspace.
