import type { CallStatDirection, IceCandidateType, IceTransport } from "./call-stats";

/**
 * Fleet-measurement telemetry for the media path one call track actually got:
 * the negotiated video codec and the succeeded ICE candidate-pair, tagged with
 * direction (publish vs subscribe) and source (camera vs screen). It is the
 * baseline the codec / Opus / ICE levers in #995 verify against, and it is what
 * surfaces the silent "stuck on TCP relay" rate.
 *
 * Pure and Faro-free: the call engine samples `summarizeVideoStats` and feeds
 * snapshots in; `telemetry.ts` owns the actual `pushEvent`. Observability only —
 * no XMPP/Jingle wire effect.
 */

/** What a call track is carrying its media as. */
type CallMediaPathSource = "camera" | "screen";

/**
 * One observed media path for a single track. `codec` and the two ICE fields
 * are null until the report names them; the sampler skips fully-empty
 * snapshots so a not-yet-negotiated track never beacons.
 */
export type CallMediaPathSnapshot = {
  direction: CallStatDirection;
  source: CallMediaPathSource;
  codec: string | null;
  iceCandidateType: IceCandidateType | null;
  iceTransport: IceTransport | null;
};

/**
 * Map a snapshot to the flat, string-valued attribute record beaconed to
 * Grafana Faro (#996). Low-cardinality and PII-free: direction, source, the
 * codec name, and the ICE candidate type + transport — never a participant
 * identity or JID. A null field becomes `"unknown"` so a partially-negotiated
 * path is still countable rather than silently dropped.
 */
export function callMediaPathEventAttributes(
  snapshot: CallMediaPathSnapshot,
): Record<string, string> {
  return {
    direction: snapshot.direction,
    source: snapshot.source,
    codec: snapshot.codec ?? "unknown",
    ice_candidate_type: snapshot.iceCandidateType ?? "unknown",
    ice_transport: snapshot.iceTransport ?? "unknown",
  };
}

/** Stateful, per-call de-duplicating beacon over observed media paths. */
export type CallMediaPathBeacon = {
  /** Report `snapshot` unless an equal one already beaconed this call. */
  observe(snapshot: CallMediaPathSnapshot): void;
  /** Forget the paths seen this call so the next call re-arms from scratch. */
  reset(): void;
};

/** Value-equality of two snapshots — any field differing is a new path. */
function sameCallMediaPath(a: CallMediaPathSnapshot, b: CallMediaPathSnapshot): boolean {
  return (
    a.direction === b.direction &&
    a.source === b.source &&
    a.codec === b.codec &&
    a.iceCandidateType === b.iceCandidateType &&
    a.iceTransport === b.iceTransport
  );
}

/**
 * Wrap a `report` sink so each *distinct* media path beacons at most once per
 * call: a recompute that yields the same path collapses to nothing, while a
 * codec switch (VP9 → VP8 backup) or an ICE re-route (relay → host) emits a
 * fresh event. `reset()` is called when a call ends so the next call starts
 * from an empty seen-set.
 *
 * The seen-set is bounded (a few directions × sources × codecs × paths), so a
 * linear scan with value-equality is both simplest and correct.
 */
export function createCallMediaPathBeacon(
  report: (snapshot: CallMediaPathSnapshot) => void,
): CallMediaPathBeacon {
  let seen: CallMediaPathSnapshot[] = [];
  return {
    observe(snapshot) {
      if (seen.some((prior) => sameCallMediaPath(prior, snapshot))) return;
      seen.push(snapshot);
      report(snapshot);
    },
    reset() {
      seen = [];
    },
  };
}
