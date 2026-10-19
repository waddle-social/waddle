# Kameo 0.20.0 peer retirement patch

Source: crates.io kameo 0.20.0, upstream https://github.com/tqwewe/kameo
(package metadata records commit 4f3e332ecb3aae730b6459d94d033c5d0c26392a
with its dirty flag set). The published crate is the authoritative baseline:
SHA-256 `b1dfd134d7a2c6ec05ee696dcbf3f7a034bdb97ecc623e981014652dcd124d77`.
Unmodified source files retain their upstream licenses; package dual-licensed
MIT OR Apache-2.0. Only library source, manifest, README and licenses retained.

Local patch exposes registry peer eviction (Kademlia routing, pending peers,
provider and metadata records) and messaging address eviction for committed
cluster departure. Remove vendoring when upstream provides these APIs.

Evicting a peer cancels outstanding messaging requests with `ConnectionClosed`,
because the remote handler may already have executed while its reply is still
in flight. A synthetic `DialFailure` would incorrectly authorize no-effect
fallback or ordered-sequence rollback. Genuine dial failures retain their
upstream classification, and excluded peers still reject late address events.

The messaging eviction method uses a scoped `#[expect(deprecated)]` because
libp2p 0.56 exposes its retained request-response address cache's eviction only
through `remove_address`; there is no replacement API in this pinned version.

The unused `otel` feature, its optional SDK hooks, and the development subscriber
dependency are removed to keep the native client's local dependency closure
free of remote telemetry capabilities. Waddle never enabled this feature;
normal actor tracing spans and the server's own telemetry remain unchanged.
