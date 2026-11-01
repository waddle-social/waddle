# Issue #1616: Restore observable LiveKit webhook ingestion

Source: https://github.com/waddle-social/waddle/issues/1616

## Implementation plan

1. Diagnose the existing namespace-to-handler delivery path using signed, effect-free diagnostic POSTs and packet/flow evidence where access permits.
2. Add real LiveKit ProtoJSON fixtures before fixing createdAt decoding; preserve signature and replay behavior.
3. Repair only the evidenced delivery boundary; prove webhook-silence alert behavior with rule tests.
4. Run focused SFU/server tests, Rust checks, rendered infrastructure checks and workspace tests; record live delivery evidence separately.

## Delivery

Use this isolated issue branch and PR. Preserve unrelated workspace changes. Keep application semantics XMPP-native, reuse existing dependencies, and record native fixture results separately from live provider or production acceptance. Do not merge or deploy as part of preparing the implementation.

## Implementation and evidence

- Diagnosed the public gateway hairpin using a signed POST originating in the
  actual SFU pod and Hubble flow evidence. The public path returned 403; a
  narrowly permitted internal Service path returned 200 and rejected an invalid
  signature with 401. The temporary policy was removed. The SFU's configured,
  projected signer and server verifier secrets were independently checked equal
  without exposing values.
- Changed only the webhook URL and its endpoint-scoped TCP/3000 egress rule.
  The in-cluster HTTP trust boundary is explicit in the operational runbook.
- Generated all four typed event fixtures with upstream livekit/protocol
  v1.45.3's actual ProtoJSON serializer. The regression failed before the fix
  with `invalid type: string "1782907200", expected i64`, then passed.
- Preserved the typed optional integer projection while accepting canonical
  decimal strings. Numeric, omitted and null forms remain accepted; malformed,
  overflowing and non-integer evidence is rejected.
- The existing signed HTTP call helpers now include canonical timestamps and
  the real webhook content type. No XMPP wire semantics changed.
- Added five silence-alert scenarios: active traffic with no accepted events,
  missing telemetry, idle calls, healthy cross-replica events and recovery.
- Added a repeatable signed diagnostic helper and sanitized runbook. The helper
  uses existing PyYAML operator tooling and adds no application dependency.
- Follow-up review found that malformed YAML diagnostics could embed credential
  source lines. The CLI now fails with a generic redacted error without chained
  parser context; two subprocess-level regression tests cover malformed YAML
  and invalid credential shapes through a fake kubectl boundary.

## Verification

- `cargo test -p waddle-sfu --locked`: **245 passed**.
- Focused webhook tests: **15 passed**, including the red/green regression.
- `cargo clippy -p waddle-sfu --all-targets --locked -- -D warnings`: passed.
- `go test ./cmd -run TestLivekitWebhookUsesPermittedInternalServerEndpoint -count=1`:
  passed; focused file-level Go vet also passed.
- All Mimir rule files and rule fixtures pass `rules/tests/run.sh` with official
  promtool 3.15.1; its release archive SHA-256 was verified.
- GitOps Kustomize rendering, Rust formatting/import-structure checks,
  diagnostic Python compilation/help and `git diff --check` passed.
- Both credential-redaction CLI tests pass after reproducing the original
  traceback failure. The exact retained live verifier also reported
  public/internal/invalid-signature HTTP 403/200/401 with temporary-policy cleanup.
- Full server workspace, signed HTTP integrations and doctests are being run by
  the integration owner after the focused checks; do not infer their result from
  the SFU tests above.

The committed configuration/parser have not been deployed. Genuine production
call lifecycle and post-rollout alert acceptance remain issue closure gates.
The synthetic probe produces operational acceptance telemetry and can suppress
the six-hour silence lookback; it is not a real-call acceptance result.

## Focused diagnostic and manifest cleanup plan

Sonar identified raw CLI context flow into optional create/delete commands,
excess complexity in the verifier and manifest test, and nested anonymous test
fixtures. Preserve the proven delivery/parser behavior and existing EOF fixes.

1. Lock the existing credential-redaction and manifest assertions; add CLI tests
   rejecting option-like/control-bearing or unknown contexts before secret reads
   or probes.
2. Delete optional policy mutation from the retained diagnostic helper. Its
   normal purpose is to inspect delivery after GitOps permits the path; the
   temporary live diagnosis is already complete and documented separately.
3. Resolve explicit contexts from local kubectl inventory and pass the selected
   inventory value as one --context=value argument. Isolate credential parsing
   so errors remain redacted and the main orchestration stays readable.
4. Give manifest fixture shapes names and flatten endpoint-rule assertions,
   keeping the same URL/selector/port acceptance contract.
5. Re-run CLI tests, focused Go tests/vet, Python compilation and whitespace
   checks; request independent review and fresh Sonar analysis.

The three CLI test methods pass, including four context rejection cases that
failed before validation was added. Credential tests also require the accepted
context to be passed atomically. Focused manifest tests and file-level Go vet
pass with named fixtures and shallow assertion helpers. No Rust production
behavior changed in this cleanup; fresh hosted Sonar and independent review
remain integration-owner gates.
