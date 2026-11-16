/// A XEP-0215 STUN or TURN service the server advertises.
public struct ExternalService: Hashable, Sendable {
    public enum Kind: Hashable, Sendable { case stun, turn, turns }
    public enum Transport: Hashable, Sendable { case udp, tcp }
    public let kind: Kind
    public let host: String
    public let port: UInt16?
    public let transport: Transport?
    public let username: String?
    public let password: String?

    public init(
        kind: Kind, host: String, port: UInt16? = nil, transport: Transport? = nil,
        username: String? = nil, password: String? = nil
    ) {
        self.kind = kind
        self.host = host
        self.port = port
        self.transport = transport
        self.username = username
        self.password = password
    }
}

/// One WebRTC ICE server entry.
public struct IceServer: Hashable, Sendable {
    public let url: String
    public let username: String?
    public let credential: String?

    public init(url: String, username: String? = nil, credential: String? = nil) {
        self.url = url
        self.username = username
        self.credential = credential
    }
}

public extension IceServer {
    /// STUN omits credentials and transport (RFC 7064). TURN needs both
    /// credentials; only TURN carries transport (RFC 7065). IPv6 hosts are
    /// bracketed, and an omitted port uses the scheme default.
    static func servers(from services: [ExternalService]) -> [IceServer] {
        services.compactMap { service in
            let host = service.host.contains(":") ? "[\(service.host)]" : service.host
            let hostPort = host + (service.port.map { ":\($0)" } ?? "")
            if service.kind == .stun { return IceServer(url: "stun:\(hostPort)") }
            guard let username = service.username, let password = service.password else { return nil }
            let scheme = service.kind == .turn ? "turn" : "turns"
            let query = service.transport.map { $0 == .udp ? "?transport=udp" : "?transport=tcp" } ?? ""
            return IceServer(url: "\(scheme):\(hostPort)\(query)", username: username, credential: password)
        }
    }
}
