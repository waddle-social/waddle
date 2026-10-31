# Authorization ID codec experiment

This isolated, unpublished crate is a testable codec contract for issue #2047.
It has its own workspace and lockfile, no production consumer, no network or
storage client, and no activation path. The current production permission
backend and its existing grants remain unchanged and potentially ambiguous.

## Contract

- `ValidatedUserJid` holds a parsed bare user JID with a localpart. `new` accepts
  a `jid::BareJid`; `parse_exact` and `from_wire` require text that survives the
  `jid` parser without a byte change. They reject full JIDs rather than stripping
  a resource. This establishes parser-validated syntax only, not canonical
  principal identity, authentication, directory resolution, or account existence.
- `OpaqueId` preserves nonempty UTF-8 bytes, including case, Unicode composition,
  punctuation, and whitespace. It never applies JID normalization or splits
  embedded `#`, `@`, or other tuple delimiters.
- Encoding is `v2_` followed by canonical unpadded base64url of the source UTF-8
  bytes. User and opaque encoded values have distinct Rust types; object type
  and userset relation must remain separate tuple fields. The codec does not
  define, infer, or authorize either field. Identical user and opaque source
  bytes deliberately encode identically: object/subject type is still required.
- Decoding rejects missing/unknown versions, padding, nonzero trailing bits,
  non-base64url characters, invalid UTF-8, empty input, and oversized input.
  It has no legacy fallback. User decoding additionally rejects text changed by
  the `jid` parser, domain-only identities, and full JIDs.
- Input is limited to **765 bytes**, not characters. With the three-byte prefix,
  765 bytes require 1023 wire bytes and 766 require 1025. The object-ID limit is
  1024 bytes in the Authzed API `ObjectReference.object_id` validation bundled
  with the existing `prescience` dependency. Some otherwise valid bare JIDs
  exceed this codec limit and are explicitly unsupported. This does not claim
  coverage of all valid XMPP JIDs.

Canonical base64url is injective on accepted source representations. It does not
establish uniqueness over logical principal identities. The pinned `jid` 0.12.2
parser accepts each of these spellings unchanged and considers each pair unequal:

- `a@example.org` and `a@example.org.`
- `a@bücher.example` and `a@xn--bcher-kva.example`

Each pair therefore produces distinct wire identifiers here. Exact regressions
record this limitation; they are not a policy that these domain aliases should
represent different users. Strict raw/wire parsing prevents a parser-induced byte
change, but cannot prove that an accepted spelling is a canonical identity.
This experiment provides no principal-equivalence or federation-readiness proof.

Errors contain no identifier data. Identifier values themselves are not
credentials and are not safe telemetry labels; this crate emits no telemetry.

## Authority and activation gate

The codec is not an authority boundary by itself. The legacy replacement codec
can produce strings beginning with `v2_`, so the prefix does **not** prevent
collisions with legacy tuples. An unknown legacy grant can collide with a new
encoded object ID even when all new IDs are injective among themselves.

Any future consumer requires a separately provisioned, isolated permission store
with explicit ownership and startup verification, plus a reviewed policy for
grant reconstruction from trustworthy authority. It also requires an authoritative
principal canonicalization contract consistent with account lookup and identity
resolution; treating this JID parser's equality as that contract is insufficient.
Canonicalization must be established before choosing stored principal keys and
must not be invented independently inside this codec. Existing ambiguous encoded
tuples must not be mechanically decoded into new grants. No legacy read fallback,
dual read, global replacement, migration, or store routing is implemented here.
These gates are unresolved; this experiment does not close issue #2047.

The codec is pure and deterministic: a failed validation returns a typed error
before any external effect. There are no claims, locks, retries, fences, or
concurrency changes. A caller cannot use codec success as authorization or
bypass the eventual store's atomicity, revision, and fencing requirements.

## Verification

From this directory, using the repository Rust toolchain:

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Tests exercise known legacy collision pairs, fixed encodings, Unicode and
delimiter preservation, validated user boundaries, unresolved domain aliases,
malformed/alternate wire
representations, byte limits, randomized injectivity, and roundtrips. These
establish the local codec contract; they do not establish live SpiceDB acceptance,
store isolation, migration safety, or production authorization correctness.
