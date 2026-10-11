# MUC effect acceptance (#1659)

An accepted room message owns one immutable, claim-fenced manifest before its ingress handled count advances. Retries execute only unfinished frozen children, including the original sender reflection.

## Authority

The canonical message envelope holds the room bare JID, authoritative room stanza ID, original claim and lifecycle/revision, occupancy incarnation, frozen complete intents, and deterministic child delivery keys. The existing ingress transaction commits this metadata, required durable effects and stream frontier together. A stale durable snapshot or an oversized manifest refuses the entire transaction.

The manifest reuses the ingress receipt authority. An opaque delivery key names an effect or an exact effect/resource tuple; it is not delivery evidence. Existing send attempts, SM custody and transactional effect receipts still establish acceptance. Existing MAM rows establish archive acceptance and do not complete unfinished siblings.

Room revisions fence initial admission. Later valid revisions do not revoke an accepted message. Local child acceptance validates the current room lifecycle and exact claim; actor-serialized callbacks validate the captured monotonic occupancy watermark. A live-roster handoff preserves the occupancy incarnation; recreation changes it. A strictly newer watermark in the same validated incarnation durably resolves only the old logical child, without enqueueing to the replacement. Missing, lower or unrelated-incarnation proofs stay pending. The watermark is a leave/rejoin replay guard, not a connection directory or lease.

The actor owns the guarded sink action, so caller cancellation cannot release a claim protector while detached storage continues. The claim transaction protects the exact room row through sink acceptance without holding the canonical row or nesting a node publication guard. A synchronous enqueue separately holds the current node guard at the actual callback. Accepted sink custody may continue draining after ownership changes.

## Child dispositions

Human endpoint copies and actual host sender-reflection consumption require their existing keyed acceptance proof. A configured host bot's ordinary occupant copy intentionally requires no transport I/O: the original planner freezes that explicit disposition. Progress reads include this policy resolution without creating a socket delivery row or successful send attempt. The aggregate route completes only after all other children finish.

Observers are independent children. Their wake hints do not complete them; the existing P1.4 durable observation work/result boundary remains authoritative. External guest/provider execution remains at least once. Offline account projections remain transactional and idempotent.

No-store and no-permanent-store sender reflections retain the original attempt's lifetime. Their frozen metadata records a storage-policy disposition before the handled frontier; this is not proof of delivery. One guarded initial reflection may enter the original sender-stream queue. Origin retries, maintenance and sibling devices cannot create another ingress reflection. Ordinary accepted-stream SM replay retains its existing behavior.

## Remote and historical boundaries

Initial remote copies retain the existing legacy wire transport. They export no new canonical manifest append key; a legacy ACK never completes the new remote child. The durable inter-node executor remains pending P2. Maintenance does not create another remote resend loop merely because this child remains pending.

Previously accepted envelopes without a manifest retain their existing authority. A retry never attaches today's occupancy snapshot as fabricated historical proof. New first room-owner acceptance seals atomically, including when the origin already established the canonical alias.

Sealed metadata is an additive storage field read by the updated server. Older strict envelope readers cannot read it: downgrade or mixed-version rollout requires draining the affected canonical rows or a separately designed migration. No protocol feature advertisement or relay wire-version change is introduced.
