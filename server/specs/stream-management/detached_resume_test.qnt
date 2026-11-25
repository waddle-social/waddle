module detached_resume_test {
  import detached_resume.* from "./detached_resume"

  run staleClaimCannotCommitTest =
    init.then(acquire(1)).then(acquire(2))
      .then(commit(1, 0).fail())

  run staleWitnessCannotPublishTest =
    init.then(acquire(1)).then(commit(1, 0)).then(observe(1))
      .then(acquire(2)).then(commit(2, 0)).then(observe(2))
      .then(publish(1).fail())

  run successiveGenerationsCanPublishTest =
    init.then(acquire(1)).then(commit(1, 0)).then(observe(1)).then(publish(1))
      .then(acquire(2)).then(commit(2, 0)).then(observe(2)).then(publish(2))
      .then(all { assert(state.publications[0].generation == 1 and state.publications[1].generation == 2), state' = state })

  run replayIncludesHydrationTailTest =
    init.then(acquire(1)).then(commit(1, 1)).then(observe(1))
      .then(admitFrame).then(publish(1))
      .then(all { assert(state.publications[0].replay == List(2, 3)), state' = state })

  run abandonedOperationCannotCommitTest =
    init.then(acquire(1)).then(abandon(1)).then(commit(1, 0).fail())

  run unexposedFrameCannotBeAcknowledgedTest =
    init.then(acquire(1)).then(commit(1, 2).fail())

  run expiredDetachedSessionCannotCommitTest =
    init.then(acquire(1)).then(deadline).then(expire).then(commit(1, 0).fail())

  run expiredHydrationCanBeReapedAfterOwnerLossTest =
    init.then(acquire(1)).then(commit(1, 0)).then(observe(1))
      .then(deadline).then(ownerLost).then(expire).then(publish(1).fail())
}
