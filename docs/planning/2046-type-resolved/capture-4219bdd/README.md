# Production compiler capture at `4219bdd71`

These artifacts record references to the selected marked declarations in the
production library targets at `4219bdd71af9fb0116c82a5c33ab1ed98eecc35d`.
They do not prove runtime reachability, semantic classification, completeness
of indirect edges, or coverage of every test and support target.

A clean detached checkout at
`/Users/rawkode/.codex/worktrees/2046-remote-ordering/waddle` was instrumented
with this PR's `mark_targets.py`. The injector verified the Git source and
produced 961 declarations across 52 files and 83 lexical handler candidates.
Handlers are candidates, not compiler or runtime coverage. From the checkout's
`server/` directory, these sequential commands exited zero and ended with
`build-finished` success:

```sh
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo check -p waddle-server --locked --offline --message-format=json
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo check -p waddle-server --features clustering --locked --offline --message-format=json
```

Their captured stdout is retained as deterministic gzip. Each stream contains
non-fresh `waddle_server` and `waddle_xmpp` non-test library artifacts. The
parser found 2,899 default and 5,059 clustering profile-specific use rows:

| Capture | Uncompressed bytes | Uncompressed SHA-256 |
| --- | ---: | --- |
| `default.jsonl.gz` | 5,133,726 | `4820e5eb2fa0847093d8a633558100e5ba6a538c7d6a9e897e9dbe0295fd985c` |
| `clustering.jsonl.gz` | 8,757,050 | `cb531e83acacd87d00bb5471e0ff50fe76f88180e5b4c25ff1c9cf9a73593c5f` |

Before instrumentation, `cargo metadata --locked --offline --no-deps
--format-version 1` was run in the same checkout's `server/` directory. Its
retained uncompressed bytes hash to
`63cc1f27ada3ecdf2a56a7dc4e28d7e52924e5ff45bb87f9e2b178439e56832f`
(158,562 bytes) and list 158 server and 98 XMPP test targets before feature
filtering. Metadata lists targets; it does not show they compiled or ran, and
its bytes alone do not attest the source commit or invocation.

The toolchain was Rust/Cargo 1.98.1 and protoc 36.2. The raw hashes preserve
captured bytes, not their authenticity or completeness. Both raw captures were
parsed with this PR's `parse_diagnostics.py`, using the recorded checkout as
both `--capture-root` and `--patched-root`, and `targets.tsv` as the manifest.
The supplied SHA-256 values are of the uncompressed streams. A separate
source-equivalent, marker-patched checkout can be passed as `--patched-root`
for reproduction; `--capture-root` remains the original recorded path.

Relative to the historical [576 capture](../capture-5763923/README.md),
7,954 old keys map uniquely to current keys after the documented owner-update
line and declaration shift, with identical diagnostic source text. Four new
clustering sites occur in the closed-readiness failure branch. Key and text
matching alone does not prove retained semantic judgments, complete caller
discovery, or behavior across changed context; those require source review.

## Test-support library, default features

On the same marked checkout, the following additional command exited zero:

```sh
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo check -p waddle-server --lib --features test-support --locked --offline --message-format=json
```

The retained stream in `test-support-default/diagnostics.jsonl.gz` has
5,686,715 uncompressed bytes and SHA-256
`04cfc6e03f2638f705efae18d49b27c6cf7d8c51e47c62ea6cd1a550f7ec64a5`.
The v2 test-capture parser accepted two expected library artifacts and found
3,251 marked references in this profile. These include production references;
they are not 3,251 unique support-only sites. The retained use, coverage and
artifact-inventory TSVs are parser output. This is compilation, not test
execution, semantic review, or complete coverage of unit/integration targets.

The equivalent command with `--features test-support,clustering` also exited
zero. `test-support-clustering/diagnostics.jsonl.gz` contains 9,309,134
uncompressed bytes with SHA-256
`b0efee8634657f6a6bca7f9b222a433ee3ff0a1ffe73b3aefc826a7464f018a4`.
The v2 parser accepted two expected library artifacts and found 5,411 marked
references including production uses. As above, this does not establish test
execution, semantic review, or full test-target coverage.

A key comparison that omits capture/profile metadata found all production
capture keys present in the corresponding support-library capture, plus 352
additional keys in each feature profile. The parser's lexical hints label 282
of those `prod`, 59 `test-support` and 11 `test`; these hints are not compiler
proof of cfg context or runtime reachability. These additional references
still require semantic review.

## One test-support integration target

After cleaning only the `waddle-server` and `waddle-xmpp` artifacts from the
same disposable target directory, this command exited zero:

```sh
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo check -p waddle-server --test supervised_io --features test-support --locked --offline --message-format=json
```

`test-support-supervised/diagnostics.jsonl.gz` has 5,864,501 uncompressed
bytes and SHA-256
`a71d09a670dd532995514204cde907ddb1cf07a855f9c28ba5b7010cb5dbbcdf`.
The v2 `server-support-integration` parser accepted the selected target and
both expected libraries, finding 3,356 marked references. Of those, 105 are
attributed to the selected `supervised_io` integration-test target, all from
its `supervised_io/connection_actor.rs` submodule; these actual warnings show
that marked code in that submodule compiled. This does not prove every
cfg-gated body was included, semantic review, test execution, other test
profiles or other integration targets.
