# Federation interoperability harness

Issue #2047, plan slice W10 ([delivery plan](../../../docs/planning/federation-2047.md)).
It runs in the verification phase (steps 6, 7 and 8): two native Waddle
deployments, the pinned Prosody and a two-node ejabberd cluster on loopback,
and a driver that walks the flow table and the negative matrix and records
PASS, FAIL or UNVERIFIED for every row in `docs/evidence/2047/`.

This is its own Cargo workspace with its own `Cargo.lock`. It depends on no
Waddle crate: it judges Waddle's wire behavior with its own XML stream reader
(`minidom`'s tree builder over `rxml`) and writer, so a codec defect in Waddle
cannot hide behind the same codec in the test.

## Running

```sh
# Static checks only (rustfmt and Clippy with -D warnings):
bash server/experiments/federation-interop/run-native.sh --check

# Everything: checks, the harness's offline tests, build waddle-server and
# the harness, run every row and write the evidence log.
bash server/experiments/federation-interop/run-native.sh

# A subset, by row-id prefix; another port range; keep the work directory:
bash server/experiments/federation-interop/run-native.sh --only flow.,obs.
bash server/experiments/federation-interop/run-native.sh --port-base 48100 --keep

# A later tree on which more slices have landed:
INTEROP_LANDED=W6a,W6b,W6c bash server/experiments/federation-interop/run-native.sh
```

The script enters `native.nix`, a test-only `nix-shell` built from the
repository's `flake.lock` (no flake output is added). The shell asserts the
pinned versions: PostgreSQL 17.11, SpiceDB 1.56.1, Prosody 13.0.6 and
ejabberd 26.07 (the plan requires 25.08 or later; the flake's nixpkgs at
`d5dfd8e6716dde34398bc14bc87c10dece9c8c68` already carries 26.07, so no
override is used). It needs the same host as the other native experiments
(16 GB of RAM for the waddle-server build), no Docker and no root.

The evidence is written as `federation-interop-<unix time>-<commit>.json`
and `.md`. Generated logs are run output, not source.

## Environment

| Piece | What the harness starts |
| --- | --- |
| CA | A `.test` CA generated at start, dated from the real clock: CA valid 30 days, leaves 7 days, one complete CRL whose `nextUpdate` is under the 7-day ceiling of Waddle's private-CA profile. Leaves are dual-usage or serverAuth-only; an unrelated CA issues the certificates nobody trusts. |
| home-a.test, home-b.test | One `waddle-server` each, with its own PostgreSQL 17 cluster (database `waddle`), its own SpiceDB on that cluster (`--datastore-engine postgres`), its own deployment UUID, session key, occupant-ID secret, dialback secret and SpiceDB key. `WADDLE_S2S_ENABLED=true` with a generated `WADDLE_S2S_CONFIG_FILE`: `peer_scope = "test"`, allowlist mode, the other home and both third parties (and the harness domains) allowlisted on the `.test` CA's private-CA profile, the other home as trusted home (and Prosody and ejabberd for the third-party moderation flows), dialback in both roles with inbound only for `peer-raw.test` and the third parties' dialback hosts, loopback configured endpoints. Accounts: `admin` (owner), `alice`, `bob`, `carol`, `caseprobe`. |
| WSS | Waddle's HTTP listener speaks plain HTTP; a deployment terminates TLS in front of it. A TLS front per deployment presents its `.test` leaf, so clients log in over WSS with the harness CA as their only root. |
| prosody.test | Prosody 13.0.6 with `s2s_secure_auth`, the `.test` CA as trust, hosts `prosody.test`, `insecure.prosody.test` (trusts only the unrelated CA, so it can only use dialback), `conference.prosody.test` (MUC with MAM) and `pubsub.prosody.test`. DNS through libunbound's forwarder to the harness's stub. |
| ejabberd.test | ejabberd 26.07 as a two-node cluster (node 2 joins node 1) with a private epmd, `s2s_use_starttls: required`, the `max_s2s_connections` and `max_s2s_connections_per_node` shaper rules at 4 and 2, hosts `ejabberd.test` and `insecure.ejabberd.test` (its `s2s_cafile` is the unrelated CA), MUC and PubSub. DNS through `ERL_INETRC` to the harness's stub. |
| Stub DNS | One view per third party: `_xmpp-server._tcp.<domain>` points at that party's relay towards the domain, `_xmpps-server` has no record. Waddle never uses it; its discovery is covered by the stub-resolver suite. |
| Relays | Every S2S path between two servers goes through its own recording relay (`net/relay.rs`). It keeps the plaintext before STARTTLS and reads the negotiated TLS version from the ServerHello; it can cut a path (step 8's partition); and in tap mode it terminates TLS on both sides with the harness's leaves, records the decrypted stream and can write a stanza into the stock server's authenticated stream. |
| Harness peer | `peer-raw.test` and `redirect.test` are the harness's own domains: an initiating server with any certificate, header and authentication (`s2s/initiator.rs`) and a receiving and authoritative server (`s2s/receiver.rs`). |

Ports are `127.0.0.1:<base + offset>` (base 47100): stub DNS +1 and +2,
home-a +10 to +14, home-b +20 to +24, Prosody +30 and +31, ejabberd +40 to
+46, relays +50 to +61, harness receivers +70 to +72, the fetch recorder
+80, the TLS bridge +81. Every service is a child in its own process group
with its log under the work directory; PostgreSQL is stopped with its fast
shutdown, the rest by a signal to the group and SIGKILL after a grace
period. The work directory holds the private keys and is removed after the
run unless `--keep` is given.

## Rows

Every row of the plan's tables is in `src/manifest.rs`. A row is PASS only
when every check it ran passed; a row whose slices have not landed, whose
peer did not start, or that the harness cannot drive is UNVERIFIED with the
reason. `manifest::LANDED` says which slices are on the tree; on
`integration/2047` as written, W5d, W5e, W5r, W6a, W6b, W6c, W7a, W7b and W8
have not landed.

Until W6c lands, what crosses S2S in both directions is server-domain
traffic: a local account's IQ to a remote entity is routed and a remote IQ
to the server domain is answered. The transport rows use it.

| Row | What drives it |
| --- | --- |
| `s2s-off.*` (step 6) | home-a with `WADDLE_S2S_ENABLED` unset and `WADDLE_S2S_CONFIG_FILE` naming a file that does not exist: it must become ready, bind nothing on its S2S port, dial nothing (relay and harness receiver stay empty), advertise nothing and export no S2S connection metrics. |
| `flow.s2s-tls-external` | Pings both ways; relays record the TLS version; Waddle's `waddle_s2s_established` metric records pkix streams. TLS 1.2 against Prosody and ejabberd by restarting them TLS-1.2-only; Waddle with Waddle negotiates 1.3, so TLS 1.2 and 1.3 in each role are shown against the harness peer. |
| `flow.dialback` | Outbound: pings to `insecure.prosody.test` and `insecure.ejabberd.test`, which cannot verify Waddle's certificate, so Waddle authenticates by dialback (Initiating role) and the peer verifies with Waddle's Authoritative role. Inbound: the harness peer without a certificate (Waddle as Receiving Server, harness as Authoritative); the stock servers present certificates and therefore authenticate with EXTERNAL, which the row records as UNVERIFIED for inbound dialback from them. |
| `flow.disco-ping` | disco#info, disco#items and ping both ways. |
| `flow.presence`, `flow.direct-messages` | Gated on W6c: subscription both ways and presence; Carbons on two home-a resources. |
| `flow.*muc*`, `flow.two-resources-*`, `flow.remote-moderator-*` | Gated on W6a/W6b (and W5e for moderation): joins, groupchat, MAM paged two at a time, two resources, a remote moderator demoting a local occupant. |
| `flow.*space*`, `flow.child-rooms-*`, `flow.bookmarks-*`, `flow.community-admin-*`, `flow.no-avatar-banner-fetch` | Gated on W7a/W7b/W8: XEP-0503 0.3.0 nodes with XEP-0402 room items, plain XEP-0060 reads, child-room joins, XEP-0402 restore on a fresh device, owner affiliation for home-b's admin, avatar and banner metadata items pointing at the harness fetch recorder (D4). Joined-space preferences and the web pilot's own fetches are UNVERIFIED here: the pilot is driven separately. |
| `obs.pre-tls-from` | The peer's first header as relayed in clear. |
| `obs.whitespace-before-header` | The tap's view of every header before and after TLS and SASL: whitespace or U+FEFF before the first header or before a restarted one. A newline between the XML declaration and the stream element is part of that header and is not counted (Waddle's own encoder writes one). |
| `obs.ejabberd-starttls` | The harness peer reading each ejabberd node's features: `<starttls><required/>`. |
| `obs.mixed-case-to` | A client of the peer pings `Home-A.TEST`. The tap shows whether the peer kept the case; if it normalized it, the stanza is written into the peer's own stream through the tap. Waddle must answer from `home-a.test`. |
| `obs.ejabberd-cluster-streams` | Clients on both nodes ping home-a in rounds; every ping must be answered while the cluster's streams replace each other; the relay counts streams for the pair. |
| `neg.cert.*` | Waddle column: the harness peer claiming home-b.test (and `muc.home-b.test` with home-b.test's leaf) with wrong-name, untrusted-issuer and expired leaves, and the harness receiving server presenting them to Waddle's outbound stream; serverAuth-only by restarting home-b on serverAuth-only leaves (both roles between the deployments) and by the harness peer; a `.test` leaf for `public-profile.test`; no pre-TLS `from`; direct TLS; TLS 1.1 and 1.0 by handing the STARTTLS socket to `openssl s_client`. G20 columns: the stock server restarted presenting the bad certificate, traffic both ways must fail; parent-domain by giving the MUC host its parent's certificate. |
| `neg.dialback.*` | The harness peer: an admitted but unlisted domain, dialback after a wrong-name certificate, `db:result` and `db:verify` on an EXTERNAL stream, extra sender and target domains, `db:verify` before STARTTLS. |
| `neg.addr.*` | Each refused form (`[::1]`, `127.0.0.1`, `2130706433`, `0x7f.1`, `127.1`, `localhost`) as header `from`, as a client's destination and as a stanza `to`; `see-other-host` before and after TLS from `redirect.test`, with a counter on the address it names. |
| `neg.route.*`, `neg.res.*` (G20) | The peer's own authenticated stream through the tap, with a stanza written into it: a spoofed `from`, a forged and a replayed IQ result, a message relayed to another domain, a 600 KiB stanza and one nested 64 deep; queue exhaustion by cutting home-a's outbound path and sending 400 pings while local traffic must still be answered. No-error-to-error, D10 and the pre-authentication element by the harness peer; account traffic refused exactly as for an account that does not exist, by the harness peer (home-b refuses account traffic itself before S2S) and by clients of the stock servers. |
| `neg.admit.*` | home-a restarted on changed snapshots: the harness peer's open stream and queued work after denylisting and after removal and `closed`; denylisted subdomains, trailing dot, case and label forms; each mode; configurations that must fail to load; subdomains of listed domains; `open` mode with `max_peers` 2 and six peers. |
| `neg.authority.*`, `neg.identity.same-room-names` | Gated on W5e/W6a (and W6b for Waddle with Waddle). |
| `restart.*` (step 8) | Each side restarted while the other stays up (ejabberd node 1 while node 2 serves), a partition by cutting both relays (outage and denial must answer differently; a local direct message must still arrive; the path must recover), and home-b killed without shutdown. |

### Not driven, with the evidence that covers them instead

- A public-CA certificate for a private-CA domain: no publicly trusted
  certificate exists offline; `s2s_verifier` inventory cases.
- Discovery (blocked address classes, changing answers, SERVFAIL, "." and
  caps, SRV-target-only certificates): `xep0368_s2s_discovery` (step 2). The
  harness uses configured endpoints by design.
- An IQ reply on a different replica: needs two replicas of one deployment;
  `s2s_iq_correlation`.
- Replica races, stale repair, scoped administration and outcast precedence:
  the W5b–W5d PostgreSQL sets and the grant-fence experiment (step 4).
- Identity collisions and reassigned JIDs: W5i's identity-profile suites and
  W5d's reuse fence.

## Known limits

- The Prosody and ejabberd configurations and the cluster join are written
  from their documentation and the shipped example and are first exercised
  in verification. A peer that does not start makes its rows UNVERIFIED
  with its log tail, never PASS.
- Stock servers present their certificates when initiating, so inbound
  dialback from them is not expected; the harness peer shows Waddle's
  Receiving role instead.
- The tap writes injected stanzas between two of the peer's own writes. A
  peer that splits one stanza across writes could have it interleaved. The
  `obs.pre-tls-from` and `obs.whitespace-before-header` rows use the tap
  only to read; `obs.mixed-case-to` injects only when the peer normalized
  the `to` itself, and says so in its `driven_by`.
- Waddle's `/metrics` is a liveness stub (its metrics leave by OTLP), so
  the harness reads stream outcomes from the deployments' S2S debug log
  (`s2s stream established` with direction and strength, `s2s service
  started`). A change to those log lines needs the harness updated.
- The harness's own C2S clients of Prosody and ejabberd check only TLS
  handshake signatures, not the chain: the certificate rows swap a whole
  host's certificate, C2S included, and what they test is S2S. Waddle's
  WSS logins and every S2S connection verify against the `.test` CA.
- A negative row first establishes a control (a known-good stream from
  the harness peer, or a ping through the tap); without it the row is
  UNVERIFIED, so a harness defect cannot pass as a refusal. Refusals are
  asserted by stage and condition: every pre-TLS refusal of a pair is
  `policy-violation`, a pre-authentication stanza is `not-authorized`, and
  a stream closed without a stream error does not count.
- The relay reads a ServerHello only when it is the first record after
  `<proceed/>` and fits one record.
