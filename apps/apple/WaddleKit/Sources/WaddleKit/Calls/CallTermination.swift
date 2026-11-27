extension CallStore {
    func hangUpForTermination() async {
        // A glare winner can take the slot before the termination task runs.
        guard hasCallToHangUpForTermination else { return }
        await hangUp()
    }

    var hasCallToHangUpForTermination: Bool {
        // Another device can still answer a call that only rang here.
        if case .incoming(let call) = state { return call.isAccepting }
        return state.isLive
    }
}
