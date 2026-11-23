// One retained stream, two resume attempts and three possible ordinals.
module detached_resume {
  type Node = int
  type Phase = Detached | Hydrating | Live | Expired
  type Receipt = Pending | Abandoned | Committed(int)
  type Witness = { generation: int, through: int, replay: List[int] }
  type Publication = {
    generation: int,
    receipt: Receipt,
    owner: Node,
    publisher: Node,
    claimHolder: Node,
    claimEpoch: int,
    ticket: int,
    replay: List[int],
    expected: List[int],
  }

  val nodes = Set(1, 2)
  val ordinals = List(1, 2, 3)

  var state: {
    phase: Phase,
    generation: int,
    owner: Node,
    claimHolder: Node,
    claimEpoch: int,
    tickets: Node -> int,
    due: bool,
    acked: int,
    exposed: int,
    appended: int,
    receipts: Node -> Receipt,
    unfencedCommit: bool,
    witnesses: Node -> Witness,
    publications: List[Publication],
  }

  action init = state' = {
    phase: Detached,
    generation: 0,
    owner: 0,
    claimHolder: 0,
    claimEpoch: 0,
    tickets: Map(1 -> 0, 2 -> 0),
    due: false,
    acked: 0,
    exposed: 1,
    appended: 2,
    receipts: Map(1 -> Pending, 2 -> Pending),
    unfencedCommit: false,
    witnesses: Map(),
    publications: List(),
  }

  def retained(acked: int, appended: int): List[int] =
    ordinals.foldl(List(), (result, n) =>
      if (acked < n and n <= appended) result.append(n) else result
    )

  def holdsClaim(node: Node): bool =
    state.claimHolder == node and state.tickets.get(node) == state.claimEpoch

  // Represents a successful claim-store CAS. A takeover invalidates the old
  // ticket even if the former claimant still has it in memory.
  action acquire(node: Node): bool = all {
    state.phase != Expired,
    state.receipts.get(node) == Pending,
    state.tickets.get(node) == 0,
    state' = {
      ...state,
      claimHolder: node,
      claimEpoch: state.claimEpoch + 1,
      tickets: state.tickets.set(node, state.claimEpoch + 1),
    },
  }

  action deadline = all {
    not(state.due),
    state.phase == Detached or state.phase == Hydrating,
    state' = { ...state, due: true },
  }

  // A hydration may lose its owner before it is published.
  action ownerLost = all {
    state.phase == Hydrating,
    state' = {
      ...state,
      phase: Detached,
      owner: 0,
      claimHolder: if (state.claimHolder == state.owner) 0 else state.claimHolder,
    },
  }

  action expire = all {
    state.phase == Detached,
    state.due,
    state' = { ...state, phase: Expired },
  }

  action admitFrame = all {
    state.phase == Detached or state.phase == Hydrating,
    state.appended < ordinals.length(),
    state' = { ...state, appended: state.appended + 1 },
  }

  // The receipt and generation change are one durable transaction. The
  // caller may discover its outcome later.
  action commit(node: Node, h: int): bool = all {
    holdsClaim(node),
    state.receipts.get(node) == Pending,
    state.phase != Expired,
    not(state.due),
    state.acked <= h and h <= state.exposed,
    state' = {
      ...state,
      phase: Hydrating,
      generation: state.generation + 1,
      owner: node,
      acked: h,
      unfencedCommit: state.unfencedCommit or not(holdsClaim(node)),
      receipts: state.receipts.set(node, Committed(state.generation + 1)),
    },
  }

  // Resolving an operation without a committed receipt durably abandons it.
  action abandon(node: Node): bool = all {
    state.tickets.get(node) > 0,
    state.receipts.get(node) == Pending,
    state' = { ...state, receipts: state.receipts.set(node, Abandoned) },
  }

  action observe(node: Node): bool = all {
    state.phase == Hydrating,
    state.owner == node,
    state.receipts.get(node) == Committed(state.generation),
    not(state.witnesses.keys().contains(node)),
    state' = {
      ...state,
      witnesses: state.witnesses.put(node, {
        generation: state.generation,
        through: state.appended,
        replay: retained(state.acked, state.appended),
      }),
    },
  }

  action publish(node: Node): bool = all {
    state.witnesses.keys().contains(node),
    state.phase == Hydrating,
    holdsClaim(node),
    state.owner == node,
    state.witnesses.get(node).generation == state.generation,
    val witness = state.witnesses.get(node)
    val hydrationTail = retained(witness.through, state.appended)
    state' = {
      ...state,
      phase: Live,
      due: false,
      exposed: state.appended,
      publications: state.publications.append({
        generation: state.generation,
        receipt: state.receipts.get(node),
        owner: state.owner,
        publisher: node,
        claimHolder: state.claimHolder,
        claimEpoch: state.claimEpoch,
        ticket: state.tickets.get(node),
        replay: witness.replay.concat(hydrationTail),
        expected: retained(state.acked, state.appended),
      }),
    },
  }

  action step = {
    nondet node = nodes.oneOf()
    nondet h = Set(0, 1, 2, 3).oneOf()
    any {
      deadline,
      expire,
      ownerLost,
      admitFrame,
      acquire(node),
      commit(node, h),
      abandon(node),
      observe(node),
      publish(node),
      state' = state,
    }
  }

  val oneCommitPerGeneration = nodes.forall(a =>
    nodes.forall(b =>
      a != b implies
        state.receipts.get(a) == Pending or
        state.receipts.get(a) == Abandoned or
        state.receipts.get(a) != state.receipts.get(b)
    )
  )

  val validPublication = state.publications.indices().forall(i => {
    val published = state.publications[i]
    published.owner == published.publisher and
    published.claimHolder == published.publisher and
    published.claimEpoch == published.ticket and
    published.receipt == Committed(published.generation) and
    published.replay == published.expected and
    (if (i == 0) true else state.publications[i - 1].generation < published.generation)
  })

  val validFrontiers =
    0 <= state.acked and state.acked <= state.exposed and state.exposed <= state.appended

  val safety =
    not(state.unfencedCommit) and oneCommitPerGeneration and validPublication and validFrontiers
}
