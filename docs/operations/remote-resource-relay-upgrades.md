# Remote resource relay upgrades

Remote resource traffic includes operations without a durable ingress obligation:
roster and blocklist pushes, room-destroy notifications, and live IQ, presence,
and MUC-proxy operations. A failed send cannot rely on ingress recovery to replay
these operations. Waiting for a peer to upgrade is also insufficient: an older
sender might target a newer receiver that permanently stopped accepting its ID.

## Wire contracts

The relay keeps independent typed receive contracts for
`waddle.clustering.relay.remote_resource_route.v8` and
`waddle.clustering.relay.remote_resource_frame.v3`. Their request and reply DTOs
live in `remote_resource_compat/wire.rs`, with explicit conversions to the current
domain types. These are the baseline contracts of the deployed release when
issue #1804 was implemented.

Non-obligation operations use separate endpoints:

- `waddle.clustering.relay.live_resource_route.v1`: full-JID routes without an
  append obligation, bare-JID routes, and MUC-proxy routes.
- `waddle.clustering.relay.live_resource_frame.v1`: outbound frames without an
  append obligation.

These live contracts do not contain the append-obligation schema. MUC admission
identity, principal, language, origin, and reply receipts retain their semantics.
Processed direct messages and obligation-bearing frames remain on the baseline
contract; converting an obligation-bearing operation to a live operation is not
permitted.

New senders first ask the live endpoint. Only `UnknownMessage` permits one
fallback ask using the baseline contract. That error is returned before the
receiver runs a handler. Older senders continue using the retained baseline
handlers on a newer receiver. Thus the installation itself can roll, and later
obligation-envelope changes do not change the live traffic contract.

`DeserializeMessage` is deliberately excluded: Kameo also returns it when the
sender cannot decode a reply after an effect committed. Reply timeouts,
connection loss, and ambiguous actor termination likewise must not trigger a
fallback. A successful reply or an application-level rejection does not trigger
a second ask. Existing stale-reference relookup and clustering cancellation
behavior still apply.

## Changing a contract

1. Keep the baseline and live wire DTOs immutable, including their nested types
   and reply shapes. Shared typed leaves are part of the wire contract too.
2. Add a new message ID and a new typed DTO for an incompatible request or reply.
   Retain the prior receiver and explicit adapters through the supported rolling
   upgrade window. Do not simply move its registration to the new ID.
3. Keep live operations on their stable endpoint when changing the durable
   obligation schema. Never erase an obligation, admission authority, or receipt
   to make a downgrade fit. A new operation that cannot be represented on the
   previous contract needs its own capability and rollout design.
4. Add a mixed-version test for each changed endpoint. Exercise both sender
   directions with the actual MessagePack codec, preserve reply/receipt fields,
   and prove ambiguous failures cannot execute the operation twice. Keep the
   baseline codec fixtures; updating them to match a changed domain type defeats
   their purpose.

The fallback has two attempts, not an unbounded retry queue. It protects the
supported mixed-version window; it does not promise delivery through unrelated
transport failures, unavailable resources, mailbox backpressure, or local
shutdown. Peers older than route v8/frame v3 are outside this baseline.

## Protocol and deployment boundaries

This changes internal relay dispatch, not XMPP wire payloads or advertised XEP
features. In particular, XEP-0198 acknowledgements still describe handled
stanzas; a compatibility failure cannot manufacture an acknowledgement or
discard a durable append obligation. XEP-0045 notifications and presence retain
their existing handlers and payloads.

Other incompatible protocols or database changes may still require `Recreate`.
The [chart's cutover guard](relay-cutovers.md) separately checks the live deployment before allowing
the return to `RollingUpdate`; a source comment or a successful image build is
not proof that the cutover image reached every replica.
