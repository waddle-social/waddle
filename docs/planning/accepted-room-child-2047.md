# Accepted room-child recovery characterization

Issue #2047 needs a precise boundary between a previously accepted room-message
child and a new protected admission. Existing room-manifest acceptance preserves
frozen children across later room revisions, but later delivery still depends on
the accepted identity, room lifecycle, occupancy and applicable claim fences.

Add focused native PostgreSQL tests through the existing production acceptance,
persisted manifest and replay/retirement seams. Characterize unchanged occupancy
after revision advancement, departure or replacement occupancy, room lifecycle
recreation, and exact child payload/target identity as appropriate. Do not
substitute the legacy roster fixture for a sealed manifest. The dedicated test
invocation must fail if its PostgreSQL prerequisite is missing; a skipped test
is not evidence.

This is characterization of the current contract, not a grant-fence integration
or a change to accepted delivery, revocation, session claims or production
activation. Report precisely which sinks and recovery paths the tests exercise.
