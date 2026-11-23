import Testing

@testable import WaddleKit

@Suite("Call activity decisions") struct CallActivityTests {
    private let audio = UserActivity(general: "talking", specific: "on_the_phone")
    private let video = UserActivity(general: "talking", specific: "on_video_phone")
    private let prior = UserActivity(general: "working", specific: "coding", text: "Swift")

    @Test func onlyEstablishedCallsHaveAMark() {
        #expect(CallActivity.mark(for: .idle) == nil)
        #expect(CallActivity.mark(for: .outgoing(.init(to: callPeer.bare, sid: "ringing", media: callAudio))) == nil)
        let direct = ActiveCall(peer: callPeer, sid: "direct", media: callAudio, join: callJoin)
        let group = GroupCall(room: groupRoom, sid: "group", media: callVideo, join: groupJoin, nick: "alice")
        #expect(CallActivity.mark(for: .active(direct)) == audio)
        #expect(CallActivity.mark(for: .inGroup(group)) == video)
    }

    @Test func claimingTheActivitySavesTheFullPriorValue() {
        let owned = CallActivity.Publication(mark: audio, previous: prior)
        #expect(CallActivity.change(desired: audio, published: prior, owned: nil) == .write(audio, keeping: owned))
        #expect(CallActivity.change(desired: audio, published: audio, owned: owned) == .keep)
        #expect(CallActivity.change(desired: nil, published: audio, owned: owned) == .write(prior, keeping: nil))
    }

    @Test func aCallWithNoPriorActivityRetractsItsMark() {
        let owned = CallActivity.Publication(mark: audio, previous: nil)
        #expect(CallActivity.change(desired: audio, published: nil, owned: nil) == .write(audio, keeping: owned))
        #expect(CallActivity.change(desired: nil, published: audio, owned: owned) == .write(nil, keeping: nil))
    }

    @Test func anUnownedCallMarkIsLeftAlone() {
        #expect(CallActivity.change(desired: audio, published: video, owned: nil) == .keep)
        #expect(CallActivity.change(desired: nil, published: video, owned: nil) == .keep)
    }

    @Test func aReplacementEndsOwnershipWithoutWriting() {
        let owned = CallActivity.Publication(mark: audio, previous: prior)
        #expect(CallActivity.change(desired: nil, published: video, owned: owned) == .forget)
        #expect(CallActivity.change(desired: nil, published: prior, owned: owned) == .forget)
        #expect(CallActivity.change(desired: nil, published: nil, owned: owned) == .forget)
        #expect(CallActivity.change(desired: video, published: prior, owned: owned) == .forget)
    }

    @Test func aNewCallKindKeepsTheOriginalActivityForRestore() {
        let owned = CallActivity.Publication(mark: audio, previous: prior)
        let updated = CallActivity.Publication(mark: video, previous: prior)
        #expect(CallActivity.change(desired: video, published: audio, owned: owned) == .write(video, keeping: updated))
    }
}
