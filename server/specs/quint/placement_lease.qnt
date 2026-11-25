// Formal protocol model for Waddle's PlacementLeaseStore CAS claim, renew,
// transfer, release, crash-reclaim, same-role SPIFFE ID with distinct
// ephemeral incarnation keys (tls-exporter binding), and fenced tenant writes.
module placement_lease {
  type Holder = { spiffeRole: str, incarnationKey: str }
  type WriteRecord = { writerKey: str, gen: int, seq: int }

  pure val H1: Holder = { spiffeRole: "cell-pool", incarnationKey: "k1" }
  pure val H2: Holder = { spiffeRole: "cell-pool", incarnationKey: "k2" }
  pure val H3: Holder = { spiffeRole: "cell-ded", incarnationKey: "k3" }
  pure val HOLDERS: Set[Holder] = Set(H1, H2, H3)
  pure val MAX_CLOCK: int = 20
  pure val LEASE_TTL: int = 4

  var clock: int
  var leaseGen: int
  var activeHolder: str
  var activeRole: str
  var leaseExpiry: int
  var fencedGen: int
  var nextWriteSeq: int
  var writeLog: Set[WriteRecord]
  var staleWriteOccurred: bool
  var staleRenewOccurred: bool

  action init = all {
    clock' = 0,
    leaseGen' = 0,
    activeHolder' = "",
    activeRole' = "",
    leaseExpiry' = 0,
    fencedGen' = 0,
    nextWriteSeq' = 0,
    writeLog' = Set(),
    staleWriteOccurred' = false,
    staleRenewOccurred' = false,
  }

  action claim(h: Holder): bool = all {
    HOLDERS.contains(h),
    activeHolder == "" or clock >= leaseExpiry,
    fencedGen' = leaseGen,
    leaseGen' = leaseGen + 1,
    activeHolder' = h.incarnationKey,
    activeRole' = h.spiffeRole,
    leaseExpiry' = clock + LEASE_TTL,
    clock' = clock,
    nextWriteSeq' = nextWriteSeq,
    writeLog' = writeLog,
    staleWriteOccurred' = staleWriteOccurred,
    staleRenewOccurred' = staleRenewOccurred,
  }

  // Renew requires exact incarnationKey (same spiffeRole with different key is denied).
  action renew(h: Holder, presentedGen: int): bool = all {
    HOLDERS.contains(h),
    activeHolder == h.incarnationKey,
    presentedGen == leaseGen,
    clock < leaseExpiry,
    leaseExpiry' = clock + LEASE_TTL,
    clock' = clock,
    leaseGen' = leaseGen,
    activeHolder' = activeHolder,
    activeRole' = activeRole,
    fencedGen' = fencedGen,
    nextWriteSeq' = nextWriteSeq,
    writeLog' = writeLog,
    staleWriteOccurred' = staleWriteOccurred,
    staleRenewOccurred' = (staleRenewOccurred or activeHolder != h.incarnationKey or presentedGen != leaseGen),
  }

  // Atomic CAS transfer fences previous generation and advances leaseGen.
  action transfer(fromH: Holder, toH: Holder, expectedGen: int): bool = all {
    HOLDERS.contains(fromH),
    HOLDERS.contains(toH),
    fromH.incarnationKey != toH.incarnationKey,
    activeHolder == fromH.incarnationKey,
    expectedGen == leaseGen,
    clock < leaseExpiry,
    fencedGen' = leaseGen,
    leaseGen' = leaseGen + 1,
    activeHolder' = toH.incarnationKey,
    activeRole' = toH.spiffeRole,
    leaseExpiry' = clock + LEASE_TTL,
    clock' = clock,
    nextWriteSeq' = nextWriteSeq,
    writeLog' = writeLog,
    staleWriteOccurred' = staleWriteOccurred,
    staleRenewOccurred' = staleRenewOccurred,
  }

  action release(h: Holder, expectedGen: int): bool = all {
    HOLDERS.contains(h),
    activeHolder == h.incarnationKey,
    expectedGen == leaseGen,
    fencedGen' = leaseGen,
    activeHolder' = "",
    activeRole' = "",
    leaseExpiry' = clock,
    clock' = clock,
    leaseGen' = leaseGen,
    nextWriteSeq' = nextWriteSeq,
    writeLog' = writeLog,
    staleWriteOccurred' = staleWriteOccurred,
    staleRenewOccurred' = staleRenewOccurred,
  }

  // Storage-host / EnteredTransaction write fence check.
  action fencedWrite(h: Holder, presentedGen: int): bool = all {
    HOLDERS.contains(h),
    presentedGen == leaseGen,
    presentedGen > fencedGen,
    activeHolder == h.incarnationKey,
    clock < leaseExpiry,
    val rec: WriteRecord = { writerKey: h.incarnationKey, gen: presentedGen, seq: nextWriteSeq }
    all {
      writeLog' = writeLog.union(Set(rec)),
      nextWriteSeq' = nextWriteSeq + 1,
      staleWriteOccurred' = (staleWriteOccurred or (presentedGen <= fencedGen) or (activeHolder != h.incarnationKey)),
      clock' = clock,
      leaseGen' = leaseGen,
      activeHolder' = activeHolder,
      activeRole' = activeRole,
      leaseExpiry' = leaseExpiry,
      fencedGen' = fencedGen,
      staleRenewOccurred' = staleRenewOccurred,
    }
  }

  action tick: bool = all {
    clock < MAX_CLOCK,
    clock' = clock + 1,
    leaseGen' = leaseGen,
    activeHolder' = activeHolder,
    activeRole' = activeRole,
    leaseExpiry' = leaseExpiry,
    fencedGen' = fencedGen,
    nextWriteSeq' = nextWriteSeq,
    writeLog' = writeLog,
    staleWriteOccurred' = staleWriteOccurred,
    staleRenewOccurred' = staleRenewOccurred,
  }

  action step = any {
    nondet h = HOLDERS.oneOf()
    claim(h),
    nondet h = HOLDERS.oneOf()
    nondet g = 0.to(leaseGen).oneOf()
    renew(h, g),
    nondet fromH = HOLDERS.oneOf()
    nondet toH = HOLDERS.oneOf()
    transfer(fromH, toH, leaseGen),
    nondet h = HOLDERS.oneOf()
    release(h, leaseGen),
    nondet h = HOLDERS.oneOf()
    nondet g = 0.to(leaseGen).oneOf()
    fencedWrite(h, g),
    tick,
  }

  // --- Safety Invariants ---
  val singleActiveHolderPerGen: bool =
    writeLog.forall(w1 => writeLog.forall(w2 => w1.gen == w2.gen implies w1.writerKey == w2.writerKey))

  val fencedHandoffOrdering: bool = all {
    activeHolder != "" implies leaseGen > fencedGen,
    writeLog.forall(w => w.gen <= leaseGen),
  }

  val sameRoleStaleReplicaDenied: bool =
    not(staleWriteOccurred) and not(staleRenewOccurred)

  val monotonicLeaseGen: bool = leaseGen >= fencedGen

  val safetyInv: bool = all {
    singleActiveHolderPerGen,
    fencedHandoffOrdering,
    sameRoleStaleReplicaDenied,
    monotonicLeaseGen,
  }

  // --- Deterministic Tests ---
  run claimRenewWriteTest = {
    init
      .then(claim(H1))
      .then(fencedWrite(H1, 1))
      .then(tick)
      .then(renew(H1, 1))
      .then(fencedWrite(H1, 1))
      .expect(writeLog.size() == 2 and leaseGen == 1 and safetyInv)
  }

  run sameRoleTransferDeniesStaleWriteTest = {
    init
      .then(claim(H1))
      .then(fencedWrite(H1, 1))
      .then(transfer(H1, H2, 1))
      .expect(leaseGen == 2 and fencedGen == 1 and activeHolder == "k2" and safetyInv)
      .then(fencedWrite(H2, 2))
      .expect(writeLog.size() == 2 and safetyInv)
      .then(fencedWrite(H1, 1).fail())
  }

  run sameRoleTransferDeniesStaleRenewTest = {
    init
      .then(claim(H1))
      .then(transfer(H1, H2, 1))
      .expect(leaseGen == 2 and activeHolder == "k2" and safetyInv)
      .then(renew(H1, 2).fail())
  }

  run expiryReclaimTest = {
    init
      .then(claim(H1))
      .then(tick).then(tick).then(tick).then(tick)
      .then(claim(H3))
      .expect(leaseGen == 2 and fencedGen == 1 and activeHolder == "k3" and safetyInv)
      .then(fencedWrite(H1, 1).fail())
  }
}
