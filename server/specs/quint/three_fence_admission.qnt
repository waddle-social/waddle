// Formal protocol model for Waddle's 3-fence inference admission,
// short-tick ReplicaFence revocation acknowledgment, racing broker replicas,
// and strict payer separation (never silently falling back from member to tenant).
module three_fence_admission {
  pure val FENCES: Set[str] = Set("cred", "room", "placement")
  pure val REPLICAS: Set[str] = Set("b1", "b2")
  pure val ATTEMPTS: Set[str] = Set("a1", "a2")
  pure val PAYERS: Set[str] = Set("user", "tenant")
  pure val MAX_GEN: int = 6

  type PendingCheck = {
    replica: str,
    attempt: str,
    payer: str,
    snapCred: int,
    snapRoom: int,
    snapPlace: int,
    permOk: bool,
  }

  type DispatchRecord = {
    attempt: str,
    payer: str,
    genCred: int,
    genRoom: int,
    genPlace: int,
  }

  var fenceOpen: str -> bool
  var fenceGen: str -> int
  var prevFenceGen: str -> int
  var userPermAllowed: bool
  var tenantPermAllowed: bool
  var revokeAcked: bool
  var revokeGen: int
  var pendingChecks: Set[PendingCheck]
  var dispatched: Set[DispatchRecord]
  var requestedPayer: str -> str
  var replicaAckedGen: str -> int
  var dispatchedAfterRevokeAck: bool
  var staleCommitOccurred: bool

  action init = all {
    fenceOpen' = Map("cred" -> true, "room" -> true, "placement" -> true),
    fenceGen' = Map("cred" -> 1, "room" -> 1, "placement" -> 1),
    prevFenceGen' = Map("cred" -> 1, "room" -> 1, "placement" -> 1),
    userPermAllowed' = true,
    tenantPermAllowed' = true,
    revokeAcked' = false,
    revokeGen' = 0,
    pendingChecks' = Set(),
    dispatched' = Set(),
    requestedPayer' = Map(),
    replicaAckedGen' = Map("b1" -> 1, "b2" -> 1),
    dispatchedAfterRevokeAck' = false,
    staleCommitOccurred' = false,
  }

  // Replica snapshots all 3 fence generations and evaluates PermissionBackend::check
  // for the explicitly requested payer (never silently switching user -> tenant).
  action startCheck(r: str, a: str, p: str): bool = all {
    REPLICAS.contains(r),
    ATTEMPTS.contains(a),
    PAYERS.contains(p),
    not(requestedPayer.keys().contains(a)) or requestedPayer.get(a) == p,
    val ok = if (p == "user") userPermAllowed else tenantPermAllowed
    val pc: PendingCheck = {
      replica: r,
      attempt: a,
      payer: p,
      snapCred: fenceGen.get("cred"),
      snapRoom: fenceGen.get("room"),
      snapPlace: fenceGen.get("placement"),
      permOk: ok,
    }
    all {
      pendingChecks' = pendingChecks.union(Set(pc)),
      requestedPayer' = if (requestedPayer.keys().contains(a)) requestedPayer else requestedPayer.put(a, p),
      fenceOpen' = fenceOpen,
      fenceGen' = fenceGen,
      prevFenceGen' = prevFenceGen,
      userPermAllowed' = userPermAllowed,
      tenantPermAllowed' = tenantPermAllowed,
      revokeAcked' = revokeAcked,
      revokeGen' = revokeGen,
      dispatched' = dispatched,
      replicaAckedGen' = replicaAckedGen,
      dispatchedAfterRevokeAck' = dispatchedAfterRevokeAck,
      staleCommitOccurred' = staleCommitOccurred,
    }
  }

  // Single atomic AdmissionLedgerStore::admit_attempt CAS:
  // admits iff permOk, all 3 fences remain open at exact snapshotted generations,
  // and attempt has not yet been dispatched.
  action commitAdmit(pc: PendingCheck): bool = all {
    pendingChecks.contains(pc),
    pc.permOk,
    fenceOpen.get("cred"),
    fenceOpen.get("room"),
    fenceOpen.get("placement"),
    fenceGen.get("cred") == pc.snapCred,
    fenceGen.get("room") == pc.snapRoom,
    fenceGen.get("placement") == pc.snapPlace,
    dispatched.forall(d => d.attempt != pc.attempt),
    val rec: DispatchRecord = {
      attempt: pc.attempt,
      payer: pc.payer,
      genCred: pc.snapCred,
      genRoom: pc.snapRoom,
      genPlace: pc.snapPlace,
    }
    val postRevokeViolation = revokeAcked and pc.payer == "user" and pc.snapCred < revokeGen
    val staleViolation = not(
      fenceOpen.get("cred") and fenceOpen.get("room") and fenceOpen.get("placement")
      and fenceGen.get("cred") == pc.snapCred
      and fenceGen.get("room") == pc.snapRoom
      and fenceGen.get("placement") == pc.snapPlace
    )
    all {
      dispatched' = dispatched.union(Set(rec)),
      pendingChecks' = pendingChecks.exclude(Set(pc)),
      dispatchedAfterRevokeAck' = (dispatchedAfterRevokeAck or postRevokeViolation),
      staleCommitOccurred' = (staleCommitOccurred or staleViolation),
      fenceOpen' = fenceOpen,
      fenceGen' = fenceGen,
      prevFenceGen' = prevFenceGen,
      userPermAllowed' = userPermAllowed,
      tenantPermAllowed' = tenantPermAllowed,
      revokeAcked' = revokeAcked,
      revokeGen' = revokeGen,
      requestedPayer' = requestedPayer,
      replicaAckedGen' = replicaAckedGen,
    }
  }

  // Close fence f and strictly advance its generation.
  action closeAndAdvanceFence(f: str): bool = all {
    FENCES.contains(f),
    fenceGen.get(f) < MAX_GEN,
    val nextGen = fenceGen.get(f) + 1
    all {
      fenceOpen' = fenceOpen.put(f, false),
      prevFenceGen' = prevFenceGen.put(f, fenceGen.get(f)),
      fenceGen' = fenceGen.put(f, nextGen),
      userPermAllowed' = if (f == "cred") false else userPermAllowed,
      tenantPermAllowed' = tenantPermAllowed,
      revokeAcked' = if (f == "cred") false else revokeAcked,
      revokeGen' = revokeGen,
      pendingChecks' = pendingChecks,
      dispatched' = dispatched,
      requestedPayer' = requestedPayer,
      replicaAckedGen' = replicaAckedGen,
      dispatchedAfterRevokeAck' = dispatchedAfterRevokeAck,
      staleCommitOccurred' = staleCommitOccurred,
    }
  }

  // Short-tick ReplicaFence sync on broker replica r.
  action replicaFenceSync(r: str): bool = all {
    REPLICAS.contains(r),
    replicaAckedGen.get(r) < fenceGen.get("cred"),
    replicaAckedGen' = replicaAckedGen.put(r, fenceGen.get("cred")),
    fenceOpen' = fenceOpen,
    fenceGen' = fenceGen,
    prevFenceGen' = prevFenceGen,
    userPermAllowed' = userPermAllowed,
    tenantPermAllowed' = tenantPermAllowed,
    revokeAcked' = revokeAcked,
    revokeGen' = revokeGen,
    pendingChecks' = pendingChecks,
    dispatched' = dispatched,
    requestedPayer' = requestedPayer,
    dispatchedAfterRevokeAck' = dispatchedAfterRevokeAck,
    staleCommitOccurred' = staleCommitOccurred,
  }

  // Revocation is acknowledged only after cred fence is closed, permission is revoked,
  // and all peer broker replicas have acknowledged the new generation via ReplicaFence.
  action ackRevoke: bool = all {
    not(fenceOpen.get("cred")),
    not(userPermAllowed),
    REPLICAS.forall(r => replicaAckedGen.get(r) >= fenceGen.get("cred")),
    not(revokeAcked),
    revokeAcked' = true,
    revokeGen' = fenceGen.get("cred"),
    fenceOpen' = fenceOpen,
    fenceGen' = fenceGen,
    prevFenceGen' = prevFenceGen,
    userPermAllowed' = userPermAllowed,
    tenantPermAllowed' = tenantPermAllowed,
    pendingChecks' = pendingChecks,
    dispatched' = dispatched,
    requestedPayer' = requestedPayer,
    replicaAckedGen' = replicaAckedGen,
    dispatchedAfterRevokeAck' = dispatchedAfterRevokeAck,
    staleCommitOccurred' = staleCommitOccurred,
  }

  action regrantUserPerm: bool = all {
    not(userPermAllowed),
    not(fenceOpen.get("cred")),
    userPermAllowed' = true,
    fenceOpen' = fenceOpen,
    fenceGen' = fenceGen,
    prevFenceGen' = prevFenceGen,
    tenantPermAllowed' = tenantPermAllowed,
    revokeAcked' = revokeAcked,
    revokeGen' = revokeGen,
    pendingChecks' = pendingChecks,
    dispatched' = dispatched,
    requestedPayer' = requestedPayer,
    replicaAckedGen' = replicaAckedGen,
    dispatchedAfterRevokeAck' = dispatchedAfterRevokeAck,
    staleCommitOccurred' = staleCommitOccurred,
  }

  action reopenFence(f: str): bool = all {
    FENCES.contains(f),
    not(fenceOpen.get(f)),
    f != "cred" or userPermAllowed,
    fenceOpen' = fenceOpen.put(f, true),
    fenceGen' = fenceGen,
    prevFenceGen' = prevFenceGen,
    userPermAllowed' = userPermAllowed,
    tenantPermAllowed' = tenantPermAllowed,
    revokeAcked' = revokeAcked,
    revokeGen' = revokeGen,
    pendingChecks' = pendingChecks,
    dispatched' = dispatched,
    requestedPayer' = requestedPayer,
    replicaAckedGen' = replicaAckedGen,
    dispatchedAfterRevokeAck' = dispatchedAfterRevokeAck,
    staleCommitOccurred' = staleCommitOccurred,
  }

  action step = any {
    nondet r = REPLICAS.oneOf()
    nondet a = ATTEMPTS.oneOf()
    nondet p = PAYERS.oneOf()
    startCheck(r, a, p),
    nondet pc = pendingChecks.oneOf()
    commitAdmit(pc),
    nondet f = FENCES.oneOf()
    closeAndAdvanceFence(f),
    nondet r = REPLICAS.oneOf()
    replicaFenceSync(r),
    ackRevoke,
    regrantUserPerm,
    nondet f = FENCES.oneOf()
    reopenFence(f),
  }

  // --- Safety Invariants ---
  val noDispatchAfterRevokeAck: bool = not(dispatchedAfterRevokeAck)

  val atMostOneDispatchPerAttempt: bool =
    ATTEMPTS.forall(a => dispatched.filter(d => d.attempt == a).size() <= 1)

  val strictGenMonotonicity: bool =
    FENCES.forall(f => fenceGen.get(f) >= prevFenceGen.get(f) and fenceGen.get(f) >= 1)

  val noSilentPayerFallback: bool =
    dispatched.forall(d => requestedPayer.keys().contains(d.attempt) and d.payer == requestedPayer.get(d.attempt))

  val failClosedOnStaleGen: bool = not(staleCommitOccurred)

  val safetyInv: bool = all {
    noDispatchAfterRevokeAck,
    atMostOneDispatchPerAttempt,
    strictGenMonotonicity,
    noSilentPayerFallback,
    failClosedOnStaleGen,
  }

  // --- Deterministic Tests ---
  run happyPathAdmitTest = {
    val pc: PendingCheck = {
      replica: "b1", attempt: "a1", payer: "user",
      snapCred: 1, snapRoom: 1, snapPlace: 1, permOk: true,
    }
    init
      .then(startCheck("b1", "a1", "user"))
      .then(commitAdmit(pc))
      .expect(dispatched.size() == 1 and safetyInv)
  }

  run revokeMidCheckDenialTest = {
    val stalePc: PendingCheck = {
      replica: "b1", attempt: "a1", payer: "user",
      snapCred: 1, snapRoom: 1, snapPlace: 1, permOk: true,
    }
    init
      .then(startCheck("b1", "a1", "user"))
      .then(closeAndAdvanceFence("cred"))
      .then(replicaFenceSync("b1"))
      .then(replicaFenceSync("b2"))
      .then(ackRevoke)
      .then(regrantUserPerm)
      .then(reopenFence("cred"))
      .expect(revokeAcked and fenceGen.get("cred") == 2 and dispatched.size() == 0 and safetyInv)
      .then(commitAdmit(stalePc).fail())
  }

  run racingReplicasSingleDispatchTest = {
    val pc1: PendingCheck = {
      replica: "b1", attempt: "a1", payer: "tenant",
      snapCred: 1, snapRoom: 1, snapPlace: 1, permOk: true,
    }
    val pc2: PendingCheck = {
      replica: "b2", attempt: "a1", payer: "tenant",
      snapCred: 1, snapRoom: 1, snapPlace: 1, permOk: true,
    }
    init
      .then(startCheck("b1", "a1", "tenant"))
      .then(startCheck("b2", "a1", "tenant"))
      .then(commitAdmit(pc1))
      .expect(dispatched.size() == 1 and safetyInv)
      .then(commitAdmit(pc2).fail())
  }
}
