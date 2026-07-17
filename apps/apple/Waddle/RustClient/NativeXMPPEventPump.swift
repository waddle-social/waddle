import Foundation

@MainActor
protocol NativeXMPPEventClient: AnyObject {
    func connect() async
    func disconnect() async
    func nextEvent() async -> WaddleClientEvent
}

extension WaddleClient: NativeXMPPEventClient {}

enum NativeXMPPEventMapping: Equatable {
    case event(XMPPEvent)
    case consumed
    case selfFence(XMPPEvent)
    case disconnected
}

enum NativeXMPPPullResult: Equatable {
    case event(XMPPEvent)
    case consumed
    case closed
}

enum NativeXMPPEventPumpState: Equatable {
    case idle
    case connecting(generation: UInt64)
    case connected(generation: UInt64)
    case closing(generation: UInt64)
    case closed
}

/// Owns the one-at-a-time native event pull boundary.
///
/// Calling code must explicitly request the next event after it finishes its
/// durability work for the current result. The pump never prefetches.
@MainActor
final class NativeXMPPEventPump {
    private let client: any NativeXMPPEventClient
    private let mapper: NativeXMPPEventMapper
    private var generation: UInt64 = 0
    private var pollTask: Task<WaddleClientEvent, Never>?
    private var closeTask: Task<Void, Never>?

    private(set) var state: NativeXMPPEventPumpState = .idle

    var isConnected: Bool {
        if case .connected = state {
            return true
        }
        return false
    }

    init(
        client: any NativeXMPPEventClient,
        mapper: NativeXMPPEventMapper
    ) {
        self.client = client
        self.mapper = mapper
    }

    func connect() async -> Bool {
        switch state {
        case .idle, .closed:
            break
        case .connecting, .connected, .closing:
            return false
        }

        generation &+= 1
        let connectGeneration = generation
        state = .connecting(generation: connectGeneration)

        await client.connect()

        guard
            generation == connectGeneration,
            state == .connecting(generation: connectGeneration)
        else {
            return false
        }

        state = .connected(generation: connectGeneration)
        return true
    }

    func nextEvent() async -> NativeXMPPPullResult {
        guard case let .connected(pollGeneration) = state else {
            return .closed
        }
        guard pollTask == nil else {
            await disconnect()
            return .event(.error("Concurrent native event polls are not allowed."))
        }

        let client = client
        let task = Task { @MainActor in
            await client.nextEvent()
        }
        pollTask = task
        let nativeEvent = await task.value
        pollTask = nil

        guard
            generation == pollGeneration,
            state == .connected(generation: pollGeneration)
        else {
            return .closed
        }

        switch mapper.map(nativeEvent) {
        case .event(let event):
            return .event(event)
        case .consumed:
            return .consumed
        case .disconnected:
            generation &+= 1
            state = .closed
            return .event(.disconnected)
        case .selfFence(let event):
            await disconnect()
            return .event(event)
        }
    }

    func disconnect() async {
        if let closeTask {
            await closeTask.value
            return
        }

        switch state {
        case .idle, .closed:
            state = .closed
            return
        case .connecting, .connected, .closing:
            break
        }

        generation &+= 1
        let closeGeneration = generation
        state = .closing(generation: closeGeneration)
        let pendingPoll = pollTask
        let client = client
        let task = Task { @MainActor [weak self] in
            await client.disconnect()
            if let pendingPoll {
                _ = await pendingPoll.value
            }
            guard
                let self,
                self.generation == closeGeneration
            else {
                return
            }
            self.pollTask = nil
            self.state = .closed
        }
        closeTask = task

        await task.value

        if generation == closeGeneration {
            closeTask = nil
        }
    }
}
