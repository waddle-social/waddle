import Testing
@testable import WaddleKit

@Suite("Verb error windows")
struct VerbErrorWindowsTests {
    @Test func coreErrorsAreAttributedByTheirVerbPrefix() {
        #expect(ErrorScopedVerb(reporting: "discover_topology failed: server_domain=x error=timeout") == .topology)
        #expect(ErrorScopedVerb(reporting: "request_avatar failed: remote-server-timeout") == .avatar)
        #expect(ErrorScopedVerb(reporting: "Invalid JID for avatar fetch: bad") == .avatar)
        #expect(ErrorScopedVerb(reporting: "send_presence failed: closed") == nil)
    }

    @Test func avatarErrorDoesNotReachTopology() {
        var windows = VerbErrorWindows()
        let topology = windows.begin(.topology)
        let avatar = windows.begin(.avatar)
        windows.record(from: .avatar)
        #expect(windows.end(topology) == false)
        #expect(windows.end(avatar) == true)
    }

    @Test func topologyErrorDoesNotReachAvatars() {
        var windows = VerbErrorWindows()
        let avatar = windows.begin(.avatar)
        let topology = windows.begin(.topology)
        windows.record(from: .topology)
        #expect(windows.end(avatar) == false)
        #expect(windows.end(topology) == true)
    }

    @Test func unattributedErrorReachesEveryOpenWindow() {
        var windows = VerbErrorWindows()
        let topology = windows.begin(.topology)
        let avatar = windows.begin(.avatar)
        windows.record(from: nil)
        #expect(windows.end(topology) == true)
        #expect(windows.end(avatar) == true)
    }

    @Test func windowsAreIndependentAndOnlySeeErrorsWhileOpen() {
        var windows = VerbErrorWindows()
        windows.record(from: nil)
        let first = windows.begin(.avatar)
        let second = windows.begin(.avatar)
        #expect(windows.end(first) == false)
        windows.record(from: .avatar)
        #expect(windows.end(second) == true)
        #expect(windows.end(first) == false)
    }
}
