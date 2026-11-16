import Testing
import WaddleKit

@Suite("ICE servers") struct IceServerTests {
    @Test func stunOmitsCredentialsAndTransport() {
        let services = [ExternalService(
            kind: .stun, host: "turn.waddle.social", port: 3478, transport: .udp,
            username: "ignored", password: "ignored")]
        #expect(IceServer.servers(from: services) == [
            IceServer(url: "stun:turn.waddle.social:3478", username: nil, credential: nil)
        ])
    }

    @Test func turnAndTurnsCarryCredentialsAndOptionalTransportInOrder() {
        let services = [
            ExternalService(kind: .turns, host: "turn.waddle.social", port: 443, transport: .tcp, username: "u", password: "p"),
            ExternalService(kind: .turn, host: "turn.waddle.social", port: 3478, transport: .udp, username: "u", password: "p"),
            ExternalService(kind: .turn, host: "turn.waddle.social", username: "", password: ""),
        ]
        #expect(IceServer.servers(from: services) == [
            IceServer(url: "turns:turn.waddle.social:443?transport=tcp", username: "u", credential: "p"),
            IceServer(url: "turn:turn.waddle.social:3478?transport=udp", username: "u", credential: "p"),
            IceServer(url: "turn:turn.waddle.social", username: "", credential: ""),
        ])
    }

    @Test func relayNeedsBothCredentials() {
        let services = [
            ExternalService(kind: .turn, host: "relay.test", username: "u"),
            ExternalService(kind: .turn, host: "relay.test", password: "p"),
            ExternalService(kind: .turns, host: "relay.test", username: "u"),
            ExternalService(kind: .turns, host: "relay.test", password: "p"),
        ]
        #expect(IceServer.servers(from: services) == [])
    }

    @Test func ipv6HostsAreBracketedAndMissingPortsAreOmitted() {
        let services = [
            ExternalService(kind: .stun, host: "2001:db8::1", port: 3478),
            ExternalService(kind: .turns, host: "2001:db8::1", transport: .tcp, username: "u", password: "p"),
            ExternalService(kind: .stun, host: "turn.waddle.social"),
        ]
        #expect(IceServer.servers(from: services) == [
            IceServer(url: "stun:[2001:db8::1]:3478"),
            IceServer(url: "turns:[2001:db8::1]?transport=tcp", username: "u", credential: "p"),
            IceServer(url: "stun:turn.waddle.social"),
        ])
    }

    @Test func noAdvertisementProducesNoServers() {
        #expect(IceServer.servers(from: []) == [])
    }
}
