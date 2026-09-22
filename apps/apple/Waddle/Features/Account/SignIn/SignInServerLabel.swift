import Foundation

enum SignInServerLabel {
    /// `host` or `host:port` for display; falls back to the full URL.
    static func text(for server: URL) -> String {
        guard let components = URLComponents(url: server, resolvingAgainstBaseURL: false),
              let host = components.host, !host.isEmpty
        else { return server.absoluteString }
        if let port = components.port {
            return "\(host):\(port)"
        }
        return host
    }
}
