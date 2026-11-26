// Formal protocol model for Waddle's broker credential lifecycle:
// envelope key custody boundary, XEP-0198 reconnect enrollment nonce idempotency,
// backup/WAL restore drill, and destruction ledger enforcement before reopening egress.
module credential_lifecycle_restore {
  pure val CREDS: Set[str] = Set("c1", "c2")
  pure val NONCES: Set[str] = Set("n1", "n2")
  pure val ALLOWED_PLAINTEXT_LOCATIONS: Set[str] = Set("client", "broker", "provider")

  var activeCiphertexts: Set[str]
  var backupCiphertexts: Set[str]
  var destructionLedger: Set[str]
  var consumedNonces: str -> str
  var egressOpen: bool
  var egressUsedWith: Set[str]
  var plaintextLocations: Set[str]
  var enrollCount: str -> int
  var usedDestroyedKey: bool

  action init = all {
    activeCiphertexts' = Set(),
    backupCiphertexts' = Set(),
    destructionLedger' = Set(),
    consumedNonces' = Map(),
    egressOpen' = true,
    egressUsedWith' = Set(),
    plaintextLocations' = Set(),
    enrollCount' = Map("n1" -> 0, "n2" -> 0),
    usedDestroyedKey' = false,
  }

  action enroll(n: str, c: str): bool = all {
    NONCES.contains(n),
    CREDS.contains(c),
    not(consumedNonces.keys().contains(n)),
    not(destructionLedger.contains(c)),
    activeCiphertexts' = activeCiphertexts.union(Set(c)),
    consumedNonces' = consumedNonces.put(n, c),
    enrollCount' = enrollCount.put(n, enrollCount.get(n) + 1),
    plaintextLocations' = plaintextLocations.union(Set("client", "broker")),
    backupCiphertexts' = backupCiphertexts,
    destructionLedger' = destructionLedger,
    egressOpen' = egressOpen,
    egressUsedWith' = egressUsedWith,
    usedDestroyedKey' = usedDestroyedKey,
  }

  // XEP-0198 reconnect replay of an already-consumed enrollment IQ nonce:
  // idempotent ack, never creates duplicate credential, never writes plaintext to cell/sm_unacked/localstorage.
  action replayEnrollIq(n: str): bool = all {
    NONCES.contains(n),
    consumedNonces.keys().contains(n),
    activeCiphertexts' = activeCiphertexts,
    backupCiphertexts' = backupCiphertexts,
    destructionLedger' = destructionLedger,
    consumedNonces' = consumedNonces,
    egressOpen' = egressOpen,
    egressUsedWith' = egressUsedWith,
    plaintextLocations' = plaintextLocations,
    enrollCount' = enrollCount,
    usedDestroyedKey' = usedDestroyedKey,
  }

  action takeBackup: bool = all {
    backupCiphertexts' = activeCiphertexts,
    activeCiphertexts' = activeCiphertexts,
    destructionLedger' = destructionLedger,
    consumedNonces' = consumedNonces,
    egressOpen' = egressOpen,
    egressUsedWith' = egressUsedWith,
    plaintextLocations' = plaintextLocations,
    enrollCount' = enrollCount,
    usedDestroyedKey' = usedDestroyedKey,
  }

  action destroyCred(c: str): bool = all {
    CREDS.contains(c),
    activeCiphertexts.contains(c),
    activeCiphertexts' = activeCiphertexts.exclude(Set(c)),
    destructionLedger' = destructionLedger.union(Set(c)),
    backupCiphertexts' = backupCiphertexts,
    consumedNonces' = consumedNonces,
    egressOpen' = egressOpen,
    egressUsedWith' = egressUsedWith,
    plaintextLocations' = plaintextLocations,
    enrollCount' = enrollCount,
    usedDestroyedKey' = usedDestroyedKey,
  }

  // Simulates restoring a stale basebackup/WAL snapshot containing previously destroyed ciphertexts.
  // Broker MUST keep egress closed until destruction ledger is applied.
  action restoreFromBackup: bool = all {
    activeCiphertexts' = backupCiphertexts,
    egressOpen' = false,
    backupCiphertexts' = backupCiphertexts,
    destructionLedger' = destructionLedger,
    consumedNonces' = consumedNonces,
    egressUsedWith' = egressUsedWith,
    plaintextLocations' = plaintextLocations,
    enrollCount' = enrollCount,
    usedDestroyedKey' = usedDestroyedKey,
  }

  action applyDestructionLedgerAndReopen: bool = all {
    not(egressOpen),
    activeCiphertexts' = activeCiphertexts.exclude(destructionLedger),
    egressOpen' = true,
    backupCiphertexts' = backupCiphertexts,
    destructionLedger' = destructionLedger,
    consumedNonces' = consumedNonces,
    egressUsedWith' = egressUsedWith,
    plaintextLocations' = plaintextLocations,
    enrollCount' = enrollCount,
    usedDestroyedKey' = usedDestroyedKey,
  }

  action useForEgress(c: str): bool = all {
    CREDS.contains(c),
    egressOpen,
    activeCiphertexts.contains(c),
    not(destructionLedger.contains(c)),
    egressUsedWith' = egressUsedWith.union(Set(c)),
    plaintextLocations' = plaintextLocations.union(Set("provider")),
    usedDestroyedKey' = (usedDestroyedKey or destructionLedger.contains(c)),
    activeCiphertexts' = activeCiphertexts,
    backupCiphertexts' = backupCiphertexts,
    destructionLedger' = destructionLedger,
    consumedNonces' = consumedNonces,
    egressOpen' = egressOpen,
    enrollCount' = enrollCount,
  }

  action step = any {
    nondet n = NONCES.oneOf()
    nondet c = CREDS.oneOf()
    enroll(n, c),
    nondet n = NONCES.oneOf()
    replayEnrollIq(n),
    takeBackup,
    nondet c = CREDS.oneOf()
    destroyCred(c),
    restoreFromBackup,
    applyDestructionLedgerAndReopen,
    nondet c = CREDS.oneOf()
    useForEgress(c),
  }

  // --- Safety Invariants ---
  val destroyedKeyUnusableAfterRestore: bool = all {
    not(usedDestroyedKey),
    egressOpen implies activeCiphertexts.intersect(destructionLedger).size() == 0,
  }

  val enrollmentNonceIdempotency: bool =
    NONCES.forall(n => enrollCount.get(n) <= 1)

  val custodyBoundary: bool =
    plaintextLocations.subseteq(ALLOWED_PLAINTEXT_LOCATIONS)

  val safetyInv: bool = all {
    destroyedKeyUnusableAfterRestore,
    enrollmentNonceIdempotency,
    custodyBoundary,
  }

  // --- Deterministic Tests ---
  run backupDestroyRestoreDrillTest = {
    init
      .then(enroll("n1", "c1"))
      .then(takeBackup)
      .then(destroyCred("c1"))
      .then(restoreFromBackup)
      .expect(not(egressOpen) and activeCiphertexts.contains("c1") and safetyInv)
      .then(applyDestructionLedgerAndReopen)
      .expect(egressOpen and not(activeCiphertexts.contains("c1")) and safetyInv)
      .then(useForEgress("c1").fail())
  }

  run egressBlockedBeforeDestructionLedgerTest = {
    init
      .then(enroll("n1", "c1"))
      .then(takeBackup)
      .then(destroyCred("c1"))
      .then(restoreFromBackup)
      .expect(not(egressOpen) and safetyInv)
      .then(useForEgress("c1").fail())
  }

  run xep0198ReplayIdempotentTest = {
    init
      .then(enroll("n1", "c1"))
      .then(replayEnrollIq("n1"))
      .then(replayEnrollIq("n1"))
      .expect(enrollCount.get("n1") == 1 and custodyBoundary and safetyInv)
  }
}
