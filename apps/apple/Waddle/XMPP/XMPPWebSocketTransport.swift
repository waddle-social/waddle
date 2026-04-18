import Foundation

enum XMPPTransportError: LocalizedError {
    case notConnected
    case invalidWebSocketURL

    var errorDescription: String? {
        switch self {
        case .notConnected:
            return "XMPP transport is not connected."
        case .invalidWebSocketURL:
            return "Invalid XMPP websocket URL."
        }
    }
}

actor XMPPWebSocketTransport {
    private let session: URLSession
    private var task: URLSessionWebSocketTask?

    init(session: URLSession = .shared) {
        self.session = session
    }

    func connect(to url: URL) throws {
        guard task == nil else {
            return
        }

        guard let scheme = URLComponents(url: url, resolvingAgainstBaseURL: false)?.scheme,
              ["ws", "wss"].contains(scheme.lowercased()) else {
            throw XMPPTransportError.invalidWebSocketURL
        }

        var request = URLRequest(url: url)
        request.setValue("xmpp", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        let nextTask = session.webSocketTask(with: request)
        task = nextTask
        nextTask.resume()
    }

    func send(_ text: String) async throws {
        guard let task else {
            throw XMPPTransportError.notConnected
        }

        try await task.send(.string(text))
    }

    func receive() async throws -> String? {
        guard let task else {
            throw XMPPTransportError.notConnected
        }

        let message = try await task.receive()
        switch message {
        case .string(let text):
            return text
        case .data(let data):
            return String(data: data, encoding: .utf8)
        @unknown default:
            return nil
        }
    }

    func close() {
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
    }
}
