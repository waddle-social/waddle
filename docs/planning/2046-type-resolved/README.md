# #2046 type-resolved connection inventory

The current production and partial test-support captures at `4219bdd71` are
documented in [`capture-4219bdd`](capture-4219bdd/README.md). The
[independent edge discovery](independent-edge-discovery-4219.md) identifies
additional paths and explicitly remains partial. The support-only library
references have grouped source notes for [actor](support-actor-review-4219.md)
and [endpoint](support-endpoint-review-4219.md) paths, plus a grouped
[supervised-I/O test-target review](supervised-io-review-4219.md). Separate
site ledgers and validators cover the 352 support-only keys in each library
profile and the 105 keys from that one supervised-I/O target. The validators
check supplied evidence, not semantic truth or test execution. Other
test-target captures and reviews, profile-specific production-site semantics,
and exhaustive indirect discovery remain open; none of these captures proves
runtime behavior.

The previous production capture remains in
[`capture-5763923`](capture-5763923/README.md), with its pinned checkpoint
scripts needed for reproduction. The historical results and commands below
describe the earlier #2286 pass and require the scripts from its pinned commit.
The current scripts target `4219bdd71` and require a separate handler output.

## Historical #2286 pass

Compiler inventory target: `main@3ae674c121c06b44934c915ecc0ef469dca54833`.
Current-main review target: `4b1fd1f348393337c827fb98ac4dea64f0f9dcf3` (#2287).
All 56 unique files in the marker manifest and selected P3 edge review are
byte-identical at both commits; their blob IDs are recorded in
[`source-provenance.tsv`](source-provenance.tsv).
This directory contains the reproducible compiler-marker pass and outputs.
Marker attributes are inserted only into a clean disposable worktree at the
pinned commit; no marker attribute belongs in serving code. The #2283 lexical
index remains documented in
[`2046-current-inventory-wiring.md`](../2046-current-inventory-wiring.md).

## Scope

The injector targets registry, endpoint and socket-state declarations and
methods; account-resource maps and actor messages; dual registration; room
routing; ordered relay bridge methods; remote-resource DTOs and compatibility
shapes; and the `OutboundStanza` producer/admission/receiver seams. The
manifest contains 851 individually identified declarations in 52 files.
Each marker tag includes its source line and column, and the injector fails
before writing if tags are not one-to-one with declarations.

The hand-reviewed P3 producer/admission/receiver edges and original-envelope
byte-budget finding are in [`P3-edge-review.md`](P3-edge-review.md). That
review distinguishes the live legacy socket endpoint from the actor-owned
candidate. A compiler reference to `ActorOutboundCustody` does not prove that
`/ws` selects it.

## Reproduction

Run from the clean #2286 source checkout. Create a separate marker worktree at
the pinned source revision, then run the injector:

```sh
git worktree add --detach /private/tmp/waddle-2046-type-inventory-scratch 3ae674c121c06b44934c915ecc0ef469dca54833
python3 docs/planning/2046-type-resolved/mark_targets.py \
  --root /private/tmp/waddle-2046-type-inventory-scratch \
  --manifest /private/tmp/waddle-2046-inventory-targets.tsv
```

The injector refuses to annotate the checkout containing itself, verifies the
scratch commit and clean state, hashes original files, records exact marker
coordinates, and validates unique tags before making any source edit. The
captured tool versions were `rustc 1.98.0 (88d9e12ae 2026-08-18)`,
`cargo 1.98.0 (797e8a9bc 2026-08-05)`, and `libprotoc 36.2`. The protobuf
compiler path was
`/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc`.

Run the two feature selections independently from the scratch worktree's
`server/` directory:

```sh
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=2 \
  cargo check -p waddle-server --locked --message-format=json \
  > /private/tmp/waddle-2046-default.jsonl \
  2> /private/tmp/waddle-2046-default.stderr
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=2 \
  cargo check -p waddle-server --features clustering --locked --message-format=json \
  > /private/tmp/waddle-2046-clustering.jsonl \
  2> /private/tmp/waddle-2046-clustering.stderr
```

These commands check the default and `clustering` production feature
selections without `--tests`, using Cargo's `dev` check profile. Both
invocations exited 0; their JSON streams ended with
`build-finished: success=true`. The parser rejects malformed JSON, missing or
failed build completion, duplicate completion, and records after completion.
It validates all pre-annotation source hashes, maps diagnostic lines back
through marker insertions, retains Cargo target kind/name/test-mode for each
warning, and writes use-site and per-declaration coverage TSVs. It reported
851 declaration targets, 2,648 default spans across 400 tags, and 4,756
clustering spans across 722 tags. The temporary compiler JSON, manifest and
Cargo target directory are not committed.

From the clean #2286 source checkout, parse default:

```sh
python3 docs/planning/2046-type-resolved/parse_diagnostics.py \
  --diagnostics /private/tmp/waddle-2046-default.jsonl --profile default \
  --patched-root /private/tmp/waddle-2046-type-inventory-scratch \
  --manifest /private/tmp/waddle-2046-inventory-targets.tsv \
  --output docs/planning/2046-type-resolved/uses-default.tsv \
  --coverage docs/planning/2046-type-resolved/coverage-default.tsv
```

Repeat with `--profile clustering`, the clustering JSON input, and the
clustering output names. The parser self-check exercises duplicate-tag,
unknown-tag, stale-hash and malformed/truncated/failed Cargo-stream rejection,
plus reconstruction of a source file without a final newline:

```sh
python3 docs/planning/2046-type-resolved/parse_diagnostics.py --self-check
```

Captured output: `parser self-check: pass`.

## Classification and limits

Each emitted marker warning is a rustc-resolved reference to a marked
declaration. `rw_classification` is deliberately
`manual-review-required`: rustc does not decide whether the use reads, writes,
performs read-modify-write, or transfers authority. All 2,648 default rows and
4,756 clustering rows (7,404 profile-specific rows total) retain that value.
The source line, enclosing function, target tag, target kind/name, target
test-mode, and profile are retained for manual review. Each compiler artifact
reports `test=false` for both `waddle_server:lib` and `waddle_xmpp:lib` in both
passes, so these rows came from normal library targets, not test harnesses.

`source_class_hint` comes from the existing lexical cfg classifier. Its
profile totals are default `prod=2,499`, `test=135`, `test-support=14` and
clustering `prod=4,605`, `test=137`, `test-support=14`. These are triage hints,
not compiler test-only classes: the classifier over-extends `#[cfg(test)]`
field attributes to containing functions. Compiler artifact `test=false` is
the target-level evidence that the two runs did not compile unit-test targets.
The compiler evidence does not establish runtime reachability, XMPP
acceptance, deployment state, or ownership correctness.

## Typed-to-lexical reconciliation

The merged #2283 candidate index reports lexical candidate rows below. The
typed counts come from the generated per-declaration coverage and use-site
TSVs. A target tag is one exact marked declaration; a use-site row is one
primary compiler span for that declaration, source location, Cargo target,
and feature profile.

| Source family | #2283 lexical rows | Target declarations | Default use sites | Clustering use sites |
| --- | ---: | ---: | ---: | ---: |
| `ConnectionRegistry` | 104 fields + 442 methods | 95 | 423 | 483 |
| `ConnectionEntry` | 485 fields + 310 methods | 72 | 401 | 471 |
| `WsConnState` | 523 fields | 50 | 564 | 564 |
| Dual registration | 17 in 7 files | 3 | 12 | 15 |
| User/resource mirrors | 199 in 43 files | 144 | 590 | 637 |
| Room dispatch | 16 in 7 files | 5 | 3 | 10 |
| Room-effect outbox | Not counted in #2283 family | 2 | 3 | 3 |
| Remote-resource/compatibility/reserved DTOs and relay handlers | 140 in 22 files | 234 | 0 | 1,567 |
| P3 outbound/endpoint/receiver seams | Not counted in #2283 family | 128 | 652 | 685 |
| Ordered relay bridge and MUC types | Not counted in #2283 family | 118 | 0 | 321 |
| **Total** | Candidate rows overlap across searches | **851** | **2,648** | **4,756** |

The lexical index intentionally retains same-named fields on unrelated types,
can have multiple matches on one line, and cannot resolve aliases, receivers,
borrows, or mutation semantics. The marker pass resolves references to the
annotated declaration but does not classify read/write authority. These units
are therefore not directly subtractable: typed spans show resolved reference
coverage and volume, not lexical false-positive counts or a complete call
graph. The remote-resource row includes compatibility and reserved DTOs;
compiler references do not mean those message IDs are registered. The P3 row
includes actor-custody candidate references and does not mean `/ws` selects
the candidate. The two profile outputs can include the same source use, so
7,404 is a sum of profile rows rather than a deduplicated source-location
count. Every row remains `manual-review-required`, and the full manual R/W
checklist remains open. Do not reuse the historical W1.9 counts as current
results.
