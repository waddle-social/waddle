# Preserve the released observer while unblocking server rollouts

Production serves revision `462cbebe` on two ready replicas. The newer
`3f387320` pod fails startup with `room observation generation has conflicting
identity`. Jev remains generation 2, but the publisher replaced its guest.

The released and rebuilt 446,651-byte guests differ in exactly 512 bytes:
16 embedded Nix dependency-store hash components. Substituting that build
path component makes the files byte-identical. This is sufficient to change
the frozen raw-byte identity without changing the observer logic.

## Recovery and implementation

1. Keep identity checks and generation 2 unchanged. Do not rewrite database
   identity, delete observation records, or cancel work through a generation bump.
2. Represent the released observer explicitly by generation, immutable OCI
   manifest digest, and Wasm payload SHA-256. Publish new server images while
   reusing that release instead of substituting an incidental rebuilt guest.
3. Verify the downloaded released payload and the rendered public execution
   policy before GitOps publication. Missing, wrong, or unavailable artifacts
   fail closed; there is no fallback to the newly built observer.
4. Retain tests of the fresh source guest and run the existing host integration
   test against the exact pinned guest too. Require the artifact to exist so
   the environment-variable skip path cannot produce false success.
5. Document intentional observer promotion: changing the guest or effective
   execution policy requires a deliberate new generation and release binding.
6. Before a live recovery, compare the candidate complete frozen identity with
   the stored generation-2 identity without emitting configuration or secret
   values. Use the normal Flux publication/reconciliation path and verify two
   updated ready replicas before declaring the rollout recovered.

## Verification

- Preserve the production startup failure and artifact comparison as the red
  reproduction. Add fixture tests for pinned selection despite a different
  fresh build, payload verification failures, and public-policy drift.
- Verify current-host loading, initialization, and invocation of the released
  guest using `jev_guest_runtime` without provider traffic.
- Run affected Python, CUE, chart-render, formatting, lint, and CI checks.
- Obtain independent architecture and final exact-diff reviews.

## Observed release

- Generation: 2.
- OCI manifest: `sha256:c37619841f9da5861887651779337fe8467f41910566267063b8b596dee379dc`.
- Wasm payload: `1978f10bffa42cc58b61bd6a8a4d4418e4a0110aa10d3958c4690c9bb10970df`.
- Read-only inspection found no live generation-2 work or pending publications.
  Completed history and published results must remain untouched.
