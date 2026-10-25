# PR #2028 review follow-up

Scope: correct public contracts and narrow poison handling identified by review.
Preserve unrelated files, authority/checkpoint fail-stop behavior, protocol shapes
and the existing decision commits. Add no dependencies.

## Cleanup plan

1. Audit repeated documentation against its actual parser, builder and consumer.
   Correct push endpoint/encryption credentials, command versus PubSub nodes,
   parsing defaults and structured summaries. Keep accurate shared descriptions.
   Strict Rustdoc validates links and lints; semantic review validates prose.
2. Independently review lock invariants before changing policy. Authority state
   and resume checkpoints must still fail closed. Disposable push caches and
   immutable client snapshots and observability throttles should use suitable existing non-poisoning
   primitives without extracting a poisoned guard or adding a dependency.
3. Lock normal cache reuse/invalidation/rate-spacing and snapshot publication
   behavior with existing tests. Add focused unwind tests through those same
   interfaces to prove one panicking operation cannot poison later operations.
   Preserve APNs minimum-refresh and limiter atomic scheduling contracts.
4. Remove server fail-stop policy from the client crate. Keep server authority
   abort immediate: logging before termination can block while authority survives.
5. Run focused tests, strict docs and Clippy; independently review the final diff.
   Commit review fixes separately, push to the existing review branch and monitor
   final-head CI. Update the PR's issue links, poison-policy rationale, trace-name
   implications and merge constraints. Resolve the fixed documentation thread.

## Delivery constraints

The repository currently permits squash merges only (`allow_rebase_merge=false`).
Changing repository merge policy or merging is outside this follow-up. Preserve
the scoped commit history on the review branch; record that rebase-merge needs a
maintainer policy change. Splitting the PR remains a separate delivery decision.
Do not reset the user's local branch before a merge exists. Keep #2018 open.

## Reviewed policy

Authority, ownership, resume checkpoints, pending invite authorization and teardown
custody keep immediate process abort on poisoning. A detached worker panic must
not leave sibling tasks serving traffic with uncertain authority. No logging is
added before abort: diagnostic I/O can block; the original panic remains logged.

APNs/FCM complete token records and the bounded VAPID JWT LRU now use safe,
non-poisoning write guards exported by the existing DashMap dependency. Its empty
`raw-api` feature exposes these lock types without adding crates. Token minting,
APNs's 20-minute minimum refresh and FCM refresh serialization stay intact. Keep
future user callbacks and multi-field partial transitions outside these guards.

The asynchronous push limiter uses Tokio's mutex; its reservations only pace
sends. Native clients publish cloned, complete snapshots using Tokio watch
replacement, so server fail-stop policy no longer applies to mobile/desktop
snapshot reads. Warning budgets, observer-history GC scheduling and recovered
counter deduplication are diagnostic state and use non-poisoning locks too.
Fenced cleanup and repair authority remain separate and fail-stop.

Existing behavior tests cover reuse, invalidation, bucket pacing and publication.
New tests inject an unwind with populated locks held, then exercise those same
interfaces. This proves continued operation for the reviewed transitions; it does
not make arbitrary future in-place mutations safe.

The module moves preserve explicit actor/message wire IDs. Kameo also derives
trace message names from Rust type paths, so private `implementation` moves can
change telemetry names. There are no known dependent dashboards. Coordinate
conflict resolution with PR #2004 before merging. Keep the decision commits
available: squash-only repository settings currently prevent rebase merge.
