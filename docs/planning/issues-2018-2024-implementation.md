# Rust reliability and API maintenance: issues 2018–2024

Status: implementation plan, 2026-10-25. Base: `6f2c65b59`.

The requested scope is #2024, #2023, #2022, #2021, #2020, #2019 and
#2018. Work stays on the current integration branch as requested. Preserve
unrelated working-copy files. Add no dependencies and change no XMPP wire format.

## Cleanup and delivery order

1. Inventory current behavior and run existing focused tests before refactoring.
   An independent architecture review checks startup, descriptor ownership,
   poisoned authority state, and compatibility of public Rust API changes.
2. #2024/#2020: remove unsafe environment mutation after threads start; make
   secret inheritance policy explicit; encapsulate libc operations using owned
   and borrowed descriptors and accurate safety invariants. Never clear the
   only bootstrap secret before persistence succeeds. A current-thread runtime
   alone does not establish single-threadedness.
3. #2019: remove poisoned-lock state recovery. Propagate typed errors where
   supported; fail closed for authority/checkpoint state and ensure background
   failures cannot leave a healthy-looking node. Add recurrence enforcement.
4. #2022: replace reference-to-option arguments, unclear boolean modes,
   fallible constructor names and getter prefixes. Add evolution and unused-value
   annotations where semantically appropriate; migrate all repository callers.
5. #2021: document the public interfaces, error contracts, panic contracts and
   modules in xmpp-core, xmpp-client, xmpp-client-wasm and extensions. Enable
   documentation lints and verify with warnings denied. Document generated APIs
   at their macro source rather than repeating comments at every expansion.
6. #2023: replace production wildcard imports; normalize one-module imports;
   extract implementation from large module facades without altering behavior.
   Cross-check PR #2004 rather than reproducing its unrelated file moves. Record
   a concrete large-file split list. Apply structural changes after other lanes
   where needed to avoid conflicting edits.
7. #2018: inspect production completion evidence read-only, using bounded queries
   and a read-only transaction. Record missing proof separately from unfinished
   dispatch; never fabricate receipts or infer loss from backlog counters.

## Verification boundaries

Existing suites establish behavior before mechanical refactors. Proposed new
regression seams are VAPID provisioning and failures, listener inheritance,
node identity and resume checkpoint after poisoning, and remote-dispatch recovery.
The assigned tickets explicitly require those public regression boundaries;
the user's instruction to implement them supplies the seam agreement. Work
proceeds in vertical test/implementation slices at those boundaries.

Each lane runs focused checks and reports exact commands and limitations. Share
the existing Rust target cache and serialize compilations to avoid duplicate
artifacts; the host has limited free disk. Run Rust formatting, workspace
typechecking, all-target/all-feature Clippy with `-D warnings`, the full native
workspace suite once after integration, workspace doctests, source guards and
affected binding/client checks. Review the integrated diff independently and
resolve actionable findings before committing the final result.

The final report distinguishes implemented acceptance criteria from operational
evidence or environmental limits and lists changed areas, simplifications and
remaining risks.
