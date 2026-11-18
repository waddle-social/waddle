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

# A tree on which the table lists slices as missing that have landed:
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
| prosody.test | Prosody 13.0.6 with `s2s_secure_auth`, the `.test` CA as trust, hosts `prosody.test`, `insecure.prosody.test` (trusts only the unrelated CA, so it can only use dialback), `conference.prosody.test` (MUC with MAM) and `pubsub.prosody.test`. `s2s_secure_auth` is global in Prosody (one policy per remote domain), so home-a is in `s2s_insecure_domains` for the dialback host; `prosody.test` offers SASL EXTERNAL only for a valid certificate, and the flows check home-a's streams with it are `pkix`. DNS through libunbound's forwarder to the harness's stub, with `do-not-query-localhost: no` and `test.` as a transparent local zone (libunbound otherwise answers that special-use zone itself). |
| ejabberd.test | ejabberd 26.07 as a two-node cluster (node 2 joins node 1) with a private epmd, `s2s_use_starttls: required`, the `max_s2s_connections` and `max_s2s_connections_per_node` shaper rules at 4 and 2, hosts `ejabberd.test` and `insecure.ejabberd.test` (its `s2s_cafile` is the unrelated CA), MUC and PubSub. DNS through `ERL_INETRC` to the harness's stub (`resolv_conf` before the stub's `nameserver`, which it would otherwise reset), with `s2s_dns_retries: 1` (ejabberd makes that many attempts in all, so zero never asks). |
| Stub DNS | One view per third party: `_xmpp-server._tcp.<domain>` points at that party's relay towards the domain, `_xmpps-server` has no record. The SRV target is the domain itself for Prosody, and `localhost` for ejabberd, which resolves SRV targets with `getaddrinfo` rather than through the stub. Waddle never uses it; its discovery is covered by the stub-resolver suite. |
| Relays | Every S2S path between two servers goes through its own recording relay (`net/relay.rs`). It keeps the plaintext before STARTTLS and reads the negotiated TLS version from the ServerHello; it can cut a path (step 8's partition); and in tap mode it terminates TLS on both sides with the harness's leaves, records the decrypted stream and can write a stanza into the stock server's authenticated stream. |
| Harness peer | `peer-raw.test` and `redirect.test` are the harness's own domains: an initiating server with any certificate, header and authentication (`s2s/initiator.rs`) and a receiving and authoritative server (`s2s/receiver.rs`). |

Ports are `127.0.0.1:<base + offset>` (base 47100): stub DNS +1 and +2,
home-a +10 to +14, home-b +20 to +24, Prosody +30 and +31, ejabberd +40 to
+46, relays +50 to +61, harness receivers +70 to +72, the fetch recorder
+80, the TLS bridge +81. Every service is a child in its own process group
with its log under the work directory; PostgreSQL is stopped with its fast
shutdown, the rest by a signal to the group and SIGKILL after a grace
period; a group is signalled only while its leader is unreaped. The work
directory is created mode 0700, holds the private keys (mode 0600) and is
removed after the run unless `--keep` is given. SIGINT, SIGTERM and SIGHUP
are caught from before the first service starts: the harness stops every
service, records each row that did not finish as UNVERIFIED, writes the
evidence and removes the work directory. The evidence names the server
binary relative to the repository, and every detail it records is scrubbed
of absolute paths, connection strings and secrets and cut to 1500
characters.

## Rows

Every row of the plan's tables is in `src/manifest.rs`. A row is PASS only
when every check it ran passed; a row whose slices have not landed, whose
peer did not start, or that the harness cannot drive is UNVERIFIED with the
reason. `manifest::LANDED` says which slices are on the tree; on
`integration/2047` every slice the rows depend on has landed (W5d, W5e,
W5r, W6a, W6b, W6c, W7a, W7b and W8), and `INTEROP_LANDED` adds slices to
a table that lists some as missing.

The transport rows use server-domain traffic, which crosses S2S in both
directions whatever else has landed: a local account's IQ to a remote
entity is routed and a remote IQ to the server domain is answered.

| Row | What drives it |
| --- | --- |
| `s2s-off.*` (step 6) | One S2S configuration file whose endpoints send home-b to its relay and `peer-raw.test` to the harness receiving server, with this row's own copies of the dialback secret and of home-a's chains and keys. Control: home-a started with S2S enabled on it listens, dials both, writes `waddle_server::s2s` log lines and is seen by the inotify watch opening the file. Then home-a with `WADDLE_S2S_ENABLED` unset and `WADDLE_S2S_CONFIG_FILE` still naming that readable file must become ready, bind nothing on its S2S port, dial neither endpoint, write no `waddle_server::s2s` log line, advertise nothing in disco#info, and open none of the watched files (inotify; if inotify is unavailable that check is UNVERIFIED). |
| `flow.s2s-tls-external` | Pings both ways; relays record the TLS version; home-a's S2S log records pkix streams established in each direction. TLS 1.2 against Prosody and ejabberd by restarting them TLS-1.2-only; Waddle with Waddle negotiates 1.3, so TLS 1.2 and 1.3 in each role are shown against the harness peer. |
| `flow.dialback` | Outbound: pings to `insecure.prosody.test` and `insecure.ejabberd.test`, which cannot verify Waddle's certificate, so Waddle authenticates by dialback (Initiating role) and the peer verifies with Waddle's Authoritative role. Inbound: the harness peer without a certificate (Waddle as Receiving Server, harness as Authoritative); the stock servers present certificates and therefore authenticate with EXTERNAL, which the row records as UNVERIFIED for inbound dialback from them. |
| `flow.disco-ping` | disco#info, disco#items and ping both ways. |
| `flow.presence`, `flow.direct-messages` | Gated on W6c: subscription both ways and presence, by clients that request their roster first (an approval goes to the interested resources, RFC 6121 §3.1.6), ending with a mutual subscription on home-a's roster; Carbons on two home-a resources. |
| `flow.*muc*`, `flow.two-resources-*`, `flow.remote-moderator-*` | Gated on W6a/W6b (and W5e for moderation): joins, groupchat, MAM paged two at a time, two resources, a remote moderator demoting a local occupant. A Waddle room is closed to other servers until an owner opens it, so the harness creates every Waddle room as an owner who submits the owner form with `urn:waddle:roomconfig:federated` on; stock rooms take their defaults. |
| `flow.*space*`, `flow.child-rooms-*`, `flow.bookmarks-*`, `flow.community-admin-*`, `flow.no-avatar-banner-fetch` | Gated on W7a/W7b/W8: XEP-0503 0.3.0 nodes with XEP-0402 room items, plain XEP-0060 reads, child-room joins, XEP-0402 restore on a fresh device, owner affiliation for home-b's admin, avatar and banner metadata items pointing at the harness fetch recorder (D4). A Waddle Space is opened to other servers by its owner (`urn:waddle:spaces:federated` in the node configuration form), and its room items are rooms of the same deployment that exist, so the harness creates them first. Joined-space preferences and the web pilot's own fetches are UNVERIFIED here: the pilot is driven separately. |
| `obs.pre-tls-from` | The peer's first header as relayed in clear. |
| `obs.whitespace-before-header` | The tap's view of every header before and after TLS and SASL: whitespace or U+FEFF before the first header or before a restarted one. A newline between the XML declaration and the stream element is part of that header and is not counted (Waddle's own encoder writes one). |
| `obs.ejabberd-starttls` | The harness peer reading each ejabberd node's features: `<starttls><required/>`. |
| `obs.mixed-case-to` | A client of the peer pings `Home-A.TEST`. The tap shows whether the peer kept the case: if it did, Waddle must answer from `home-a.test`; if it normalized it, a message to `alice@Home-A.TEST` is written into the peer's own stream through the tap, and home-a's alice must receive it addressed to `home-a.test` (an injected IQ would not do: its answer goes to a peer that never sent the request, and a Waddle peer drops such an uncorrelated response). |
| `obs.ejabberd-cluster-streams` | Clients on both nodes ping home-a in rounds; every ping must be answered while the cluster's streams replace each other; the relay counts streams for the pair. |
| `neg.cert.*` | Every certificate refusal must appear in home-a's log as `s2s peer certificate refused` with the verifier's code (`identity_mismatch` for a wrong name or a parent-domain certificate, `path` for an untrusted issuer or an expired certificate). In the stock columns a refusal under another code means the server did not present the certificate under test, and the check is UNVERIFIED. Waddle column: the harness peer claiming home-b.test (and `muc.home-b.test` with home-b.test's leaf) with wrong-name, untrusted-issuer and expired leaves, refused at TLS; outbound, a control in which home-a dials the harness receiving server and delivers with the good certificate, then the receiving server presenting the bad one on a fresh stream; serverAuth-only by restarting home-b on serverAuth-only leaves (both roles between the deployments) and by the harness peer; a `.test` leaf for `public-profile.test`; no pre-TLS `from`; direct TLS; TLS 1.1 and 1.0 by handing the STARTTLS socket to `openssl s_client`, with TLS 1.2 through the same bridge as the control. G20 columns: disco#info and ping both ways with the good certificate first; the stock server restarted presenting the bad certificate, then shown listening for S2S and answering its own client; traffic both ways must then fail; parent-domain by giving the MUC host its parent's certificate. |
| `neg.dialback.*` | The harness peer. The unlisted-domain and after-failure rows first establish dialback for the listed `peer-raw.test`. An admitted but unlisted domain (`redirect.test`, whose authoritative server would verify the key) must be refused with `policy-violation` before any verification request, with home-a logging the connection ended with its own stream error (the domain is admitted, so this is not an admission outcome); dialback after a wrong-name certificate must be refused at TLS with the `identity_mismatch` refusal logged and no verification request; on an EXTERNAL stream, an unsolicited `db:result` request and a `db:verify` answer nothing asked for must end the stream, while a `db:verify` request for a key home-a did not issue is answered `invalid` (the Authoritative Server keeps no state to correlate it with); extra sender and target domains on EXTERNAL and dialback streams, each after a ping within the pair is answered on the same stream; `db:verify` before STARTTLS. |
| `neg.addr.*` | Each refused form (`[::1]`, `127.0.0.1`, `2130706433`, `0x7f.1`, `127.1`, `localhost`) as header `from`, as a client's destination and as a stanza `to`. As a destination, after a control in which a good destination is dialled, each form must be answered with an error while no harness listener and nothing on loopback port 5269 sees a connection. `see-other-host` before and after TLS from `redirect.test`: the harness records that it sent it, home-a must log the connection ended as redirected, and the address it names must see no connection. |
| `neg.route.*`, `neg.res.*` (G20) | The peer's own authenticated stream through the tap, with a stanza written into it: a spoofed `from`, a forged and a replayed IQ result, a message relayed to another domain, a 600 KiB stanza and one nested 64 deep. After a forged or replayed result, or a relayed message that home-a dropped, a client of the peer pings home-a and the answer must come on the same tapped connection after the injected bytes, so home-a read past them; if the ping takes another connection the follow-up is UNVERIFIED. The replay is written into the stream that carried the genuine result, or the row is UNVERIFIED. The relayed message is watched for on the third server by a client first shown to receive a message by id. Queue exhaustion stalls home-a's outbound relay (connections accepted, never answered: Waddle's queue against a stalled socket, not the stock server) and sends 400 pings while local traffic must still be answered. No-error-to-error, D10 and the pre-authentication element by the harness peer; a vCard request to an account's bare JID, a handler outside W6c's reviewed set, refused with `service-unavailable` exactly as for an account that does not exist, by the harness peer (home-b refuses it itself before S2S) and by clients of the stock servers, each after a ping to home-a on the same path is answered. A refusal is read on the tapped connection the stanza was written into, not the newest one: the peer opens a new stream as soon as home-a closes one. |
| `neg.admit.*` | home-a restarted on changed snapshots: the harness peer's open stream and queued work after denylisting and after removal and `closed`; denylisted subdomains, trailing dot, case and label forms; each mode; configurations that must fail to load; subdomains of listed domains. Slot exhaustion: `open` mode with `max_peers` 2, eviction after 5 s without traffic and a 60 s first backoff, every relay into home-a cut, and peers admitted by `open` mode alone. Exactly two peers must establish; the third is refused with `resource-constraint` and logged as ended for capacity; after the idle time a fourth evicts exactly one of the two (logged as evicted); and a destination whose outbound attempt failed is not dialled again within the backoff. |
| `neg.authority.*`, `neg.identity.same-room-names` | Gated on W5e/W6a (and W6b for Waddle with Waddle). Private discovery and MAM are refused for a members-only room only after the same client was answered about an open room and read its archive, and a refusal must come from the room's host, not as `remote-server-not-found` or `remote-server-timeout`. Moderation on a dialback-only stream, a grant stored outside the canonical path and a former admin of a removed home are rows without a driver yet, UNVERIFIED with what the driver needs. |
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
  no row reads metrics: the harness reads stream outcomes from the
  deployments' JSON S2S debug log (`s2s stream established` with direction
  and strength, `s2s connection ended` with its outcome, `s2s peer
  certificate refused` with its code, `s2s service started`, and the
  `waddle_server::s2s` target). A change to those log lines needs the
  harness updated; the offline tests pin the needles against literal lines.
- The harness's own C2S clients of Prosody and ejabberd check only TLS
  handshake signatures, not the chain: the certificate rows swap a whole
  host's certificate, C2S included, and what they test is S2S. Waddle's
  WSS logins and every S2S connection verify against the `.test` CA.
- A negative row first establishes a control on the same path (a
  known-good stream from the harness peer, a delivery with the good
  certificate, a ping through the tap); without it the row is UNVERIFIED,
  so a harness defect cannot pass as a refusal. Where it can, a refusal is
  also tied to home-a's own log line for it. Refusals are
  asserted by stage and condition: every pre-TLS refusal of a pair is
  `policy-violation`, a pre-authentication stanza is `not-authorized`, and
  a stream closed without a stream error does not count.
- The relay reads a ServerHello only when it is the first record after
  `<proceed/>` and fits one record.
