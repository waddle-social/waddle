# MUC effect acceptance (#1659)

On a single-node deployment, an accepted room message owns one immutable manifest before its ingress handled count advances. Retries execute only unfinished frozen children, including the original sender reflection. Clustered deployments retain the existing keyed delivery path until P2 supplies durable remote completion; new clustered messages are not sealed.

## Authority

The canonical message envelope holds the room bare JID, authoritative room stanza ID, original claim and lifecycle/revision, occupancy incarnation, frozen complete intents, and deterministic child delivery keys. The existing ingress transaction commits this metadata, required durable effects and stream frontier together. A stale durable snapshot or an oversized manifest refuses the entire transaction.

The manifest reuses the ingress receipt authority. An opaque delivery key names an effect or an exact effect/resource tuple; it is not delivery evidence. Existing send attempts, SM custody and transactional effect receipts still establish acceptance. Existing MAM rows establish archive acceptance and do not complete unfinished siblings.

Room revisions fence initial admission. Later valid revisions do not revoke an accepted message. Local child acceptance validates the current room lifecycle and exact claim; actor-serialized callbacks validate the captured monotonic occupancy watermark. A live-roster handoff preserves the occupancy incarnation; recreation changes it. The watermark is a leave/rejoin replay guard, not a connection directory or lease.

Single-node recovery retires only the original child when an actor-serialized check proves departure, a strictly newer occupancy, or a published replacement lifecycle/incarnation. A registry-serialized absence check can retire a destroyed room only when no published room or pending ownership/preparation work remains. Missing registries, actor timeouts, and lower watermarks prove nothing. Child policy reasons are stored in the existing delivery progress table, separately from transport acceptance; retirement cannot relabel previously accepted custody or settle healthy siblings.

The actor owns the guarded sink action, so caller cancellation cannot release a claim protector while detached storage continues. The claim transaction protects the exact room row through sink acceptance without holding the canonical row or nesting a node publication guard. A synchronous enqueue separately holds the current node guard at the actual callback. Accepted sink custody may continue draining after ownership changes.

## Child dispositions

Human endpoint copies and actual host sender-reflection consumption require their existing keyed acceptance proof. A configured host bot's ordinary occupant copy intentionally requires no transport I/O: the original planner freezes that explicit disposition. Progress reads include this policy resolution without creating a socket delivery row or successful send attempt. The aggregate route completes only after all other children finish.

Observers are independent children. Their wake hints do not complete them; the existing P1.4 durable observation work/result boundary remains authoritative. External guest/provider execution remains at least once. Offline account projections remain transactional and idempotent.

No-store and no-permanent-store sender reflections retain the original attempt's lifetime. Their frozen metadata records a storage-policy disposition before the handled frontier; this is not proof of delivery. One guarded initial reflection may enter the original sender-stream queue. Origin retries, maintenance and sibling devices cannot create another ingress reflection. Ordinary accepted-stream SM replay retains its existing behavior.

## Remote and historical boundaries

New clustered submissions retain the existing exported append key, receiver deduplication, ACK completion, archive readiness, and reflection rules. This prevents unfinished experimental remote children from accumulating in deployed traffic. Cross-node manifest execution is dark only: a unit-test scope can opt into sealing, but no production setting can enable it.

Existing sealed rows always retain their frozen authority, including in a clustered process. The new-acceptance gate never downgrades them to legacy rows. Experimental sealed remote children remain nonterminal pending P2, and their legacy wire ACKs do not acquire durable manifest acceptance authority. Such rows must not be introduced into a production database before P2.

Sealed ghost repair probes only frozen children, pinning the exact incarnation, watermark, and session generation before reachability checks. It retains a generation-specific ordinary leave sweep before eviction, then obtains a separate policy retirement proof. Thus a cancelled sweep can be retried, an empty-room destroy can retire sibling rows, and a rejoined occupant cannot inherit the old eviction. Eviction alone never proves transport acceptance. Legacy unsealed recovery retains its existing settlement path.

Previously accepted envelopes without a manifest retain their existing authority. A retry never attaches today's occupancy snapshot as fabricated historical proof. New first room-owner acceptance seals atomically, including when the origin already established the canonical alias.

## Storage and deployment

Unsealed messages retain the exact strict V1 envelope shape. Sealed messages use V2, which requires a manifest. Readers reject a manifest encoded as V1, a missing V2 manifest, and unknown versions. The manifest decoder validates frozen children against the versioned intent and delivery-key codecs; changes to those codecs require a new storage version or an explicit migration.

Migration V1026 permits envelope versions 1/2 and adds child policy reasons without introducing a second ledger. The deployed HelmRelease uses Recreate for this cutover. Release automation then derives a new cutover floor; restore RollingUpdate only in a separate change without server changes after the guard confirms every ready pod has the new reader and migration support.

The clustered gate keeps newly accepted fleet rows compatible with V1 payload readers, but it does not make a database containing experimental sealed rows readable by old images. This rollout assumes #1945 has not previously been deployed. An experimental database containing sealed V1 rows must be handled explicitly before upgrading; it must not be silently treated as unsealed authority. Image-only rollback across V1026 is unsupported because the previous migration catalog rejects the newer database version. Use a database snapshot plus the matching image, or a separately verified forward repair. No new protocol advertisement or relay wire-version change is introduced.
