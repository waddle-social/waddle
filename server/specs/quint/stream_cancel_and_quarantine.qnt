// Formal protocol model for Waddle's multi-turn budget ledger,
// unknown_after_send quarantine (zero re-billing), bounded stream permits,
// and synchronous Drop / ReplicaFence stream cancellation within SLA ticks.
module stream_cancel_and_quarantine {
  pure val STEPS: Set[str] = Set("s1", "s2", "s3")
  pure val COSTS: Set[int] = Set(10, 25, 40)
  pure val SIGNED_CAP: int = 100
  pure val MAX_PERMITS: int = 2
  pure val CANCEL_SLA_TICKS: int = 3
  pure val MAX_CLOCK: int = 20

  var spent: int
  var reserved: int
  var attemptState: str -> str
  var attemptReserved: str -> int
  var attemptSendCount: str -> int
  var cancelRequestedAt: str -> int
  var cancelCompletedAt: str -> int
  var clock: int
  var permitsInUse: int

  action init = all {
    spent' = 0,
    reserved' = 0,
    attemptState' = Map("s1" -> "idle", "s2" -> "idle", "s3" -> "idle"),
    attemptReserved' = Map("s1" -> 0, "s2" -> 0, "s3" -> 0),
    attemptSendCount' = Map("s1" -> 0, "s2" -> 0, "s3" -> 0),
    cancelRequestedAt' = Map("s1" -> -1, "s2" -> -1, "s3" -> -1),
    cancelCompletedAt' = Map("s1" -> -1, "s2" -> -1, "s3" -> -1),
    clock' = 0,
    permitsInUse' = 0,
  }

  action reserveAndSend(s: str, estCost: int): bool = all {
    STEPS.contains(s),
    estCost > 0,
    attemptState.get(s) == "idle",
    spent + reserved + estCost <= SIGNED_CAP,
    permitsInUse < MAX_PERMITS,
    attemptState' = attemptState.put(s, "streaming"),
    attemptReserved' = attemptReserved.put(s, estCost),
    attemptSendCount' = attemptSendCount.put(s, attemptSendCount.get(s) + 1),
    reserved' = reserved + estCost,
    permitsInUse' = permitsInUse + 1,
    spent' = spent,
    cancelRequestedAt' = cancelRequestedAt,
    cancelCompletedAt' = cancelCompletedAt,
    clock' = clock,
  }

  action reconcileSuccess(s: str, actualCost: int): bool = all {
    STEPS.contains(s),
    attemptState.get(s) == "streaming",
    actualCost >= 0,
    actualCost <= attemptReserved.get(s),
    reserved' = reserved - attemptReserved.get(s),
    spent' = spent + actualCost,
    attemptReserved' = attemptReserved.put(s, 0),
    attemptState' = attemptState.put(s, "committed"),
    permitsInUse' = permitsInUse - 1,
    attemptSendCount' = attemptSendCount,
    cancelRequestedAt' = cancelRequestedAt,
    cancelCompletedAt' = cancelCompletedAt,
    clock' = clock,
  }

  // Ambiguous upstream acceptance: conservatively charge reserved budget and
  // transition to terminal "quarantined" state. Never retries or re-bills.
  action markUnknownAfterSend(s: str): bool = all {
    STEPS.contains(s),
    attemptState.get(s) == "streaming",
    reserved' = reserved - attemptReserved.get(s),
    spent' = spent + attemptReserved.get(s),
    attemptReserved' = attemptReserved.put(s, 0),
    attemptState' = attemptState.put(s, "quarantined"),
    permitsInUse' = permitsInUse - 1,
    attemptSendCount' = attemptSendCount,
    cancelRequestedAt' = cancelRequestedAt,
    cancelCompletedAt' = cancelCompletedAt,
    clock' = clock,
  }

  action requestCancel(s: str): bool = all {
    STEPS.contains(s),
    attemptState.get(s) == "streaming",
    cancelRequestedAt.get(s) == -1,
    cancelRequestedAt' = cancelRequestedAt.put(s, clock),
    spent' = spent,
    reserved' = reserved,
    attemptState' = attemptState,
    attemptReserved' = attemptReserved,
    attemptSendCount' = attemptSendCount,
    cancelCompletedAt' = cancelCompletedAt,
    clock' = clock,
    permitsInUse' = permitsInUse,
  }

  action completeCancel(s: str, partialCost: int): bool = all {
    STEPS.contains(s),
    attemptState.get(s) == "streaming",
    cancelRequestedAt.get(s) >= 0,
    partialCost >= 0,
    partialCost <= attemptReserved.get(s),
    reserved' = reserved - attemptReserved.get(s),
    spent' = spent + partialCost,
    attemptReserved' = attemptReserved.put(s, 0),
    attemptState' = attemptState.put(s, "canceled"),
    cancelCompletedAt' = cancelCompletedAt.put(s, clock),
    permitsInUse' = permitsInUse - 1,
    attemptSendCount' = attemptSendCount,
    cancelRequestedAt' = cancelRequestedAt,
    clock' = clock,
  }

  // Clock advances only while active cancel requests remain within the bounded SLA window.
  action tick: bool = all {
    clock < MAX_CLOCK,
    STEPS.forall(s =>
      (attemptState.get(s) == "streaming" and cancelRequestedAt.get(s) >= 0)
        implies (clock + 1 - cancelRequestedAt.get(s) <= CANCEL_SLA_TICKS)
    ),
    clock' = clock + 1,
    spent' = spent,
    reserved' = reserved,
    attemptState' = attemptState,
    attemptReserved' = attemptReserved,
    attemptSendCount' = attemptSendCount,
    cancelRequestedAt' = cancelRequestedAt,
    cancelCompletedAt' = cancelCompletedAt,
    permitsInUse' = permitsInUse,
  }

  action step = any {
    nondet s = STEPS.oneOf()
    nondet c = COSTS.oneOf()
    reserveAndSend(s, c),
    nondet s = STEPS.oneOf()
    reconcileSuccess(s, attemptReserved.get(s)),
    nondet s = STEPS.oneOf()
    markUnknownAfterSend(s),
    nondet s = STEPS.oneOf()
    requestCancel(s),
    nondet s = STEPS.oneOf()
    completeCancel(s, 0),
    tick,
  }

  // --- Safety Invariants ---
  val budgetConservation: bool = all {
    spent >= 0,
    reserved >= 0,
    spent + reserved <= SIGNED_CAP,
  }

  val zeroRebillingOnQuarantine: bool =
    STEPS.forall(s => attemptState.get(s) == "quarantined" implies attemptSendCount.get(s) == 1)

  val permitBounds: bool = permitsInUse >= 0 and permitsInUse <= MAX_PERMITS

  val boundedCancelLatency: bool =
    STEPS.forall(s =>
      cancelCompletedAt.get(s) >= 0 implies
        (cancelCompletedAt.get(s) >= cancelRequestedAt.get(s)
         and cancelCompletedAt.get(s) - cancelRequestedAt.get(s) <= CANCEL_SLA_TICKS)
    )

  val safetyInv: bool = all {
    budgetConservation,
    zeroRebillingOnQuarantine,
    permitBounds,
    boundedCancelLatency,
  }

  // --- Deterministic Tests ---
  run quarantineNeverRetriesTest = {
    init
      .then(reserveAndSend("s1", 40))
      .then(markUnknownAfterSend("s1"))
      .expect(attemptState.get("s1") == "quarantined" and attemptSendCount.get("s1") == 1 and permitsInUse == 0 and spent == 40 and safetyInv)
      .then(reserveAndSend("s1", 10).fail())
  }

  run dropCancelPropagatesTest = {
    init
      .then(reserveAndSend("s1", 30))
      .then(requestCancel("s1"))
      .then(tick)
      .then(completeCancel("s1", 10))
      .expect(attemptState.get("s1") == "canceled" and permitsInUse == 0 and spent == 10 and reserved == 0 and safetyInv)
  }
}
