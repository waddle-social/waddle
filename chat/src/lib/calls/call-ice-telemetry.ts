import type { IceCandidateType, IceTransport } from "./call-stats";

/**
 * ICE/TURN fleet telemetry for calls (#1452).
 *
 * `call-media-path-telemetry.ts` already reports the *per-track* media path
 * (codec + succeeded candidate pair). This module answers the three
 * connectivity questions that track-scoped view cannot:
 *
 * 1. **Relay vs direct** — is media flowing peer↔SFU directly, or being
 *    hairpinned through TURN? A fleet-wide climb in `relay` is a
 *    NAT/firewall or SFU-reachability regression, and relay paths cost
 *    latency and TURN bandwidth.
 * 2. **TURN reachability** — did ICE gather a `relay` candidate at all? A
 *    client that never gathers one has no fallback; when the direct path
 *    also fails the call simply never connects. This is invisible in the
 *    succeeded-pair view precisely because there is no succeeded pair.
 * 3. **ICE restarts** — how often the transport had to re-negotiate. A
 *    call that connects but restarts repeatedly reads as "connected" in
 *    every other signal while being unusable.
 *
 * Pure and Faro-free: the call engine feeds raw stats reports and transport
 * phase transitions in; `telemetry.ts` owns the actual `pushEvent`.
 * Observability only — no XMPP/Jingle wire effect.
 *
 * Privacy: candidate *types* and coarse buckets only. No IP addresses, no
 * ports, no candidate foundations, no TURN server hostnames — an
 * `RTCIceCandidate`'s address fields never leave this module's inputs.
 */

/** Whether media is hairpinned through TURN or reaching the SFU directly. */
export type IcePathClass = "relay" | "direct" | "unknown";

/**
 * Whether ICE managed to gather a TURN `relay` candidate this call.
 * `not-gathered` means the client has no fallback path — the interesting
 * failure mode, since it only bites when the direct path also fails.
 */
/** `"unknown"` means relay availability was not measurable from this
 * report: either it carried no local-candidate entries at all (gathering
 * not started, mid-reconnect), or a direct pair won — track-scoped
 * `getRTCStatsReport()` can omit candidates unreferenced by the selected
 * pair, so a healthy direct call must not read as "no TURN fallback".
 * `"not-gathered"` is reserved for the alert-worthy case: candidates were
 * gathered, none was a relay, and no direct path succeeded. */
type TurnReachability = "gathered" | "not-gathered" | "unknown";

/** Coarse, low-cardinality bucket over the ICE restart count. */
export type IceRestartBucket = "none" | "once" | "few" | "many";

/** Lifecycle signal for expiring XEP-0215 TURN credentials. */
export type IceCredentialEvent = "refreshed" | "expired";

/**
 * One observed ICE state for a call. Every field is a closed enum or a
 * bucket, so the beacon stays low-cardinality and de-dupes usefully.
 */
export type CallIceSnapshot = {
  pathClass: IcePathClass;
  candidateType: IceCandidateType | null;
  transport: IceTransport | null;
  turnReachability: TurnReachability;
  restartBucket: IceRestartBucket;
};

/**
 * Minimal structural view of the `RTCStats` entries this module reads.
 * Browsers disagree on which fields exist, so everything is optional and
 * an absent field simply yields `null` rather than a wrong answer.
 */
type IceStatLike = {
  id?: string;
  type?: string;
  selectedCandidatePairId?: string;
  selected?: boolean;
  nominated?: boolean;
  state?: string;
  localCandidateId?: string;
  candidateType?: string;
  protocol?: string;
  relayProtocol?: string;
};

/** Narrow a raw `candidateType` to the four ICE types, else null. */
function normalizeCandidateType(value: string | undefined): IceCandidateType | null {
  return value === "host" || value === "srflx" || value === "prflx" || value === "relay"
    ? value
    : null;
}

/** `tls` is TCP-based, so it folds into `tcp` — operationally the same leg. */
function normalizeTransport(value: string | undefined): IceTransport | null {
  const normalized = value?.toLowerCase();
  if (normalized === "udp") return "udp";
  if (normalized === "tcp" || normalized === "tls") return "tcp";
  return null;
}

/**
 * Classify the succeeded candidate type as relay vs direct. `srflx`/`prflx`
 * are direct: the media still flows peer↔SFU, STUN only discovered the
 * address. Only `relay` costs a TURN hop.
 */
export function icePathClass(candidateType: IceCandidateType | null): IcePathClass {
  if (candidateType === null) return "unknown";
  return candidateType === "relay" ? "relay" : "direct";
}

/**
 * Bucket a restart count. Three non-zero buckets is enough to separate
 * "one blip" from "the path is thrashing" without turning a monotonically
 * growing counter into unbounded attribute values.
 */
export function iceRestartBucket(count: number): IceRestartBucket {
  if (count <= 0) return "none";
  if (count === 1) return "once";
  if (count <= 3) return "few";
  return "many";
}

/**
 * Pick the active candidate pair, mirroring the selector in `call-stats.ts`:
 * the transport's `selectedCandidatePairId` is spec-correct, then Firefox's
 * `selected` flag, then a nominated *and* succeeded pair, then any succeeded
 * pair. `nominated` alone is unreliable (a failed pair can stay nominated),
 * so it must never shadow the path media is flowing over.
 */
function selectPair(pairs: IceStatLike[], selectedId: string | undefined): IceStatLike | undefined {
  if (selectedId !== undefined) {
    const selected = pairs.find((pair) => pair.id === selectedId);
    if (selected) return selected;
  }
  return (
    pairs.find((pair) => pair.selected) ??
    pairs.find((pair) => pair.nominated && pair.state === "succeeded") ??
    pairs.find((pair) => pair.state === "succeeded") ??
    pairs.find((pair) => pair.nominated)
  );
}

/**
 * Reduce one raw stats report to the ICE observation, given the call's
 * running restart count.
 *
 * TURN reachability is read from *every* `local-candidate` entry, not just
 * the succeeded pair's: the whole point is to see that a relay candidate
 * was gathered even when a direct pair won (the healthy case) and — more
 * importantly — that none was gathered when nothing won at all.
 */
export function summarizeIceStats(
  report: Iterable<unknown>,
  restartCount: number,
): CallIceSnapshot {
  const pairs: IceStatLike[] = [];
  const byId = new Map<string, IceStatLike>();
  let selectedPairId: string | undefined;
  let relayGathered = false;
  let localCandidatesSeen = false;

  for (const raw of report) {
    // Browsers ship partial, version-skewed `RTCStats` dictionaries; the
    // structural view above is read through optional fields, matching the
    // same pattern `call-stats.ts` uses.
    const stat = raw as IceStatLike;
    if (stat.id !== undefined) byId.set(stat.id, stat);
    if (stat.type === "transport" && stat.selectedCandidatePairId !== undefined) {
      selectedPairId = stat.selectedCandidatePairId;
    } else if (stat.type === "candidate-pair") {
      pairs.push(stat);
    } else if (stat.type === "local-candidate") {
      localCandidatesSeen = true;
      if (stat.candidateType === "relay") relayGathered = true;
    }
  }

  const pair = selectPair(pairs, selectedPairId);
  const local = pair?.localCandidateId !== undefined ? byId.get(pair.localCandidateId) : undefined;
  const candidateType = normalizeCandidateType(local?.candidateType);
  // For relay, only `relayProtocol` (the client↔TURN leg) is meaningful — the
  // candidate's own `protocol` is the TURN↔SFU leg and would mask a TCP/TLS
  // fallback. Absent relayProtocol → null, never a false `udp`.
  const rawTransport = candidateType === "relay" ? local?.relayProtocol : local?.protocol;

  const pathClass = icePathClass(candidateType);
  // "not-gathered" requires positive evidence of failure-without-relay:
  // candidates were gathered, none was a relay, and no candidate pair
  // succeeded. A succeeded pair whose local-candidate fields were
  // unmeasurable (pathClass "unknown") is a working call, not a missing
  // TURN fallback.
  // A selected pair with a MISSING state still counts as established
  // (Firefox omits fields), but an explicitly failed one never does —
  // a failed selected pair with no relay is the real "no path, no TURN".
  const pathEstablished =
    pair?.state === "succeeded" ||
    (selectedPairId !== undefined && pair !== undefined && pair.state !== "failed");
  return {
    pathClass,
    candidateType,
    transport: normalizeTransport(rawTransport),
    turnReachability: relayGathered
      ? "gathered"
      : localCandidatesSeen && !pathEstablished
        ? "not-gathered"
        : "unknown",
    restartBucket: iceRestartBucket(restartCount),
  };
}

/**
 * Map a snapshot to the flat, string-valued attribute record beaconed to
 * Grafana Faro. Low-cardinality and PII-free: candidate types and buckets
 * only — never an IP, port, candidate foundation, or TURN hostname.
 */
export function callIceEventAttributes(snapshot: CallIceSnapshot): Record<string, string> {
  return {
    ice_path: snapshot.pathClass,
    ice_candidate_type: snapshot.candidateType ?? "unknown",
    ice_transport: snapshot.transport ?? "unknown",
    turn_reachability: snapshot.turnReachability,
    ice_restarts: snapshot.restartBucket,
  };
}

/**
 * Collect an `RTCStatsReport`'s entries into a plain iterable. The report is
 * Map-like but its value type is browser-defined, so this is the single seam
 * where the untyped shape enters — everything downstream reads the structural
 * {@link IceStatLike} view.
 */
export function iceStatsEntries(report: { forEach(fn: (value: unknown) => void): void }): unknown[] {
  const entries: unknown[] = [];
  report.forEach((value) => entries.push(value));
  return entries;
}

/** Stateful, per-call de-duplicating beacon over observed ICE states. */
export type CallIceBeacon = {
  /** Record an ICE restart; the next observation carries the new bucket. */
  noteIceRestart(): void;
  /** Beacon a TURN credential lifecycle transition for this call. */
  noteCredentials(event: IceCredentialEvent): void;
  /** The restart count observed so far this call. */
  restartCount(): number;
  /** Reduce `report` and beacon it unless an equal state already beaconed. */
  observe(report: Iterable<unknown>): void;
  /** Forget this call's ICE state so the next call re-arms from scratch. */
  reset(): void;
};

function sameIceSnapshot(a: CallIceSnapshot, b: CallIceSnapshot): boolean {
  return (
    a.pathClass === b.pathClass &&
    a.candidateType === b.candidateType &&
    a.transport === b.transport &&
    a.turnReachability === b.turnReachability &&
    a.restartBucket === b.restartBucket
  );
}

/**
 * Wrap a `report` sink so each *distinct* ICE state beacons at most once per
 * call: repeated polls of a settled path collapse to nothing, while a
 * re-route (direct → relay), a late-gathered TURN candidate, or crossing a
 * restart-bucket boundary emits a fresh event.
 *
 * The seen-set is bounded by the product of the five closed enums, so a
 * linear scan with value-equality is both simplest and correct.
 */
export function createCallIceBeacon(
  report: (snapshot: CallIceSnapshot) => void,
  reportCredentials: (event: IceCredentialEvent) => void = () => undefined,
): CallIceBeacon {
  let seen: CallIceSnapshot[] = [];
  let restarts = 0;
  return {
    noteIceRestart() {
      restarts += 1;
    },
    noteCredentials(event) {
      reportCredentials(event);
    },
    restartCount() {
      return restarts;
    },
    observe(statsReport) {
      const snapshot = summarizeIceStats(statsReport, restarts);
      // A report with no path and no candidate data measured nothing —
      // beaconing it would ship a false "no TURN" signal mid-gathering.
      if (snapshot.pathClass === "unknown" && snapshot.turnReachability === "unknown") return;
      if (seen.some((prior) => sameIceSnapshot(prior, snapshot))) return;
      seen.push(snapshot);
      report(snapshot);
    },
    reset() {
      seen = [];
      restarts = 0;
    },
  };
}
