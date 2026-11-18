# Compiler capture at `5763923e`

These artifacts record source references for the production library targets at
`5763923e0cb15d3bbab9cf53800e5b1bdd41e183`. They do not classify the uses,
prove runtime reachability, or cover every test and indirect actor edge.
`handlers.tsv` is a fixed-scope lexical candidate list, not compiler coverage.

The disposable source checkout was
`/Users/rawkode/.codex/worktrees/2046-inventory-scratch/waddle`. The injector
added temporary markers to 52 files and emitted 961 targets. A later lexical
scan of all `server/crates` Rust files produced the retained 83 explicit
handler candidates, including four test candidates omitted from the initial
79-candidate scan. The `server/` toolchain selects `rustc 1.98.1 (48a229cea
2026-09-01)` and `cargo 1.98.1 (797e8a9bc 2026-08-05)`; `protoc 36.2` was
used. Both commands below
exited zero and their captured streams ended with `build-finished` success.

```sh
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=2 cargo check -p waddle-server --locked --message-format=json > /private/tmp/w2046-refresh-default.jsonl 2> /private/tmp/w2046-refresh-default.stderr
PROTOC=/nix/store/jxrris8n0dw7am1kzs3ygfqyd9l6s4l3-protobuf-36.2/bin/protoc CARGO_TARGET_DIR=/private/tmp/waddle-2046-type-inventory-target CARGO_BUILD_JOBS=2 cargo check -p waddle-server --features clustering --locked --message-format=json > /private/tmp/w2046-refresh-clustering.jsonl 2> /private/tmp/w2046-refresh-clustering.stderr
```

Both captured `waddle_server` and `waddle_xmpp` non-test library artifacts
were rebuilt, using the dev profile. The default pass has no features on
either library; the clustering pass has `clustering` on the server library and
no features on the XMPP library. The parser produced 2,899 default and 5,055
clustering reference rows. These are profile-specific rows, not unique calls.

The deterministic gzip files retain the raw stdout bytes. SHA-256 of their
**uncompressed** streams, recorded after each command returned:

- `default.jsonl.gz`: `196173d35f2848fcd2a4bd6a9cdfb96efb5c20570c08a234a653d2867d289bd8` (5,160,046 bytes).
- `clustering.jsonl.gz`: `9bfc5d312bef980a1e180b1e94fa353b4f54d3ec82ba0c46e2584ff1ea62ce6b` (8,841,522 bytes).

`cargo-metadata.json.gz` retains the uncompressed output of `cargo metadata
--locked --offline --no-deps --format-version 1` from the same checkout's
`server/` directory. Its uncompressed SHA-256 is
`c37bcac7c270c53de0e20c5baa4624638106f3435d9d4a119ddec0894fa6c309`
(159,342 bytes). It lists 158 server and 98 XMPP integration-test targets
before feature filtering. This is a target inventory, not evidence those tests
were compiled or run.

The hashes detect changes to the retained bytes; they do not independently
prove capture completeness or authenticity. `uses-*.tsv` and `coverage-*.tsv`
are deterministic parser outputs. A full semantic validation also requires
site, handler, and indirect-edge reviews, and an exact source-tree comparison.
The retained Cargo artifacts contain the original absolute checkout path.
Reparsing supplies that path as `--capture-root` and independently validates a
pinned annotated checkout supplied as `--patched-root`; the two may differ.

To reparse these historical streams, use the injector and parser from signed
checkpoint `95a1f378c15ce2f41fa9029f7096cf9861796206`, whose scripts are
pinned to this source. Later versions of these scripts may target a newer
commit. Create a clean disposable checkout at the source commit and run the
commands from a separate checkout of that checkpoint. For example, set
`INVENTORY_SCRATCH` to the absolute disposable source checkout path:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 docs/planning/2046-type-resolved/mark_targets.py --root "$INVENTORY_SCRATCH" --manifest /private/tmp/w2046-targets.tsv --handlers /private/tmp/w2046-handlers.tsv
PYTHONDONTWRITEBYTECODE=1 python3 docs/planning/2046-type-resolved/parse_diagnostics.py --diagnostics docs/planning/2046-type-resolved/capture-5763923/default.jsonl.gz --capture-sha256 196173d35f2848fcd2a4bd6a9cdfb96efb5c20570c08a234a653d2867d289bd8 --profile default --capture-root /Users/rawkode/.codex/worktrees/2046-inventory-scratch/waddle --patched-root "$INVENTORY_SCRATCH" --manifest /private/tmp/w2046-targets.tsv --output /private/tmp/w2046-uses-default.tsv --coverage /private/tmp/w2046-coverage-default.tsv
PYTHONDONTWRITEBYTECODE=1 python3 docs/planning/2046-type-resolved/parse_diagnostics.py --diagnostics docs/planning/2046-type-resolved/capture-5763923/clustering.jsonl.gz --capture-sha256 9bfc5d312bef980a1e180b1e94fa353b4f54d3ec82ba0c46e2584ff1ea62ce6b --profile clustering --capture-root /Users/rawkode/.codex/worktrees/2046-inventory-scratch/waddle --patched-root "$INVENTORY_SCRATCH" --manifest /private/tmp/w2046-targets.tsv --output /private/tmp/w2046-uses-clustering.tsv --coverage /private/tmp/w2046-coverage-clustering.tsv
```

Compare those outputs and the generated manifests with the retained TSVs.
The parser and inventory validator reject mismatched hashes, source trees,
profiles, and incomplete supplied reviews; neither proves that manual edge
discovery or semantic classifications are complete or correct.
