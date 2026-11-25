import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

struct CallReactionsTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)
    private let ada = JID(parsing: "ada@waddle.test/web")!
    private let room = BareJID(parsing: "design@rooms.waddle.test")!
    private let join = CallJoin(url: "wss://sfu", room: "r", identity: "me@waddle.test/mac", token: "t")
    private let audio = CallMedia(audio: true, video: false)

    @Test func aReactionShowsForAMomentAndGoes() {
        var ledger = CallReactionLedger()
        ledger.received("👍", from: ada, at: now)
        #expect(ledger.bursts.map(\.emoji) == ["👍"])
        #expect(ledger.nextExpiry == now.addingTimeInterval(CallReactionLedger.lifetime))
        ledger.expire(at: now.addingTimeInterval(CallReactionLedger.lifetime - 0.01))
        #expect(ledger.bursts.count == 1)
        ledger.expire(at: now.addingTimeInterval(CallReactionLedger.lifetime))
        #expect(ledger.bursts.isEmpty)
        #expect(ledger.nextExpiry == nil)
    }

    /// Anyone in a room can send these as fast as they like: what is on the stage changes only so often.
    @Test func onePastTheLimitIsNotShown() {
        var ledger = CallReactionLedger()
        for index in 0..<(CallReactionLedger.limit + 5) {
            let sender = JID(parsing: "design@rooms.waddle.test/p\(index)")!
            ledger.received("🎉", from: sender, at: now.addingTimeInterval(Double(index) * 0.01))
        }
        #expect(ledger.bursts.count == CallReactionLedger.limit)
        #expect(ledger.bursts.map(\.id) == Array(1...CallReactionLedger.limit), "the first are shown out; a later one waits for nothing")
        // When they have gone there is room again.
        ledger.received("🎉", from: ada, at: now.addingTimeInterval(CallReactionLedger.lifetime + 1))
        #expect(ledger.bursts.count == 1)
    }

    @Test func nobodyFillsTheStageAlone() {
        var ledger = CallReactionLedger()
        for _ in 0..<10 { ledger.received("👍", from: ada, at: now) }
        #expect(ledger.bursts.count == CallReactionLedger.limitEach)
        ledger.received("👍", from: JID(parsing: "linus@waddle.test/web")!, at: now)
        #expect(ledger.bursts.count == CallReactionLedger.limitEach + 1)
    }

    @Test func eachIsSaidWithWhoseItIs() {
        var ledger = CallReactionLedger()
        ledger.received("👍", from: JID(parsing: "design@rooms.waddle.test/Ada")!, at: now)
        ledger.received("🎉", from: ada, at: now)
        ledger.received("🔥", from: nil, at: now)
        #expect(ledger.bursts[0].spoken(ownNick: "me") == "Ada reacted 👍")
        #expect(ledger.bursts[0].spoken(ownNick: "Ada") == "You reacted 👍", "a room hands our own back under our nick")
        #expect(ledger.bursts[1].spoken(ownNick: nil) == "ada reacted 🎉")
        #expect(ledger.bursts[2].spoken(ownNick: nil) == "You reacted 🔥")
    }

    @Test func twoAtOnceRiseInDifferentPlaces() {
        var ledger = CallReactionLedger()
        ledger.received("👍", from: ada, at: now)
        ledger.received("👍", from: nil, at: now)
        let lanes = ledger.bursts.map(\.lane)
        #expect(abs(lanes[0] - lanes[1]) > 0.2)
        #expect(lanes.allSatisfy { (0..<1).contains($0) })
    }

    @Test(arguments: ["👍", "❤️", "😂", "🔥", "👏", "🎉", "👨‍👩‍👧", "🇳🇴", "👍🏽", "1️⃣", "✌🏽", "🏴󠁧󠁢󠁥󠁮󠁧󠁿"])
    func oneEmojiIsShown(_ emoji: String) {
        #expect(CallReactionLedger.isOneEmoji(emoji))
    }

    /// The wire carries whatever the sender put there.
    @Test(arguments: ["", " ", "a", "1", "#", "hello", "👍👍", "👍 ", "<b>", "buy now"])
    func anythingElseIsNot(_ text: String) {
        #expect(!CallReactionLedger.isOneEmoji(text))
        var ledger = CallReactionLedger()
        ledger.received(text, from: ada, at: now)
        #expect(ledger.bursts.isEmpty)
    }

    /// One "character" can be made of hundreds of parts, and draws as a wall of them.
    @Test func aSingleCharacterBuiltToBeHugeIsNotAnEmoji() {
        let stacked = "👍" + String(repeating: "\u{0301}", count: 300)
        let joined = Array(repeating: "👨", count: 200).joined(separator: "\u{200D}")
        let toned = "👍" + String(repeating: "🏽", count: 200)
        for text in [stacked, joined, toned] {
            #expect(!CallReactionLedger.isOneEmoji(text))
        }
    }

    @Test func thePickersSixAreAllShown() {
        #expect(CallReactionLedger.offered.count == 6)
        #expect(CallReactionLedger.offered.allSatisfy(CallReactionLedger.isOneEmoji))
    }

    @Test func inACallOfTwoItIsTheOtherDevicesAndThisCalls() {
        let state = CallState.active(ActiveCall(peer: ada, sid: "s1", media: audio, join: join))
        let reaction = InCallReaction(sid: "s1", emoji: "👍")
        #expect(CallReactionLedger.belongs(reaction, from: ada, isGroupchat: false, to: state))
        #expect(!CallReactionLedger.belongs(InCallReaction(sid: "other", emoji: "👍"), from: ada, isGroupchat: false, to: state))
        #expect(!CallReactionLedger.belongs(reaction, from: JID(parsing: "ada@waddle.test/phone")!, isGroupchat: false, to: state))
        #expect(!CallReactionLedger.belongs(reaction, from: JID(parsing: "eve@waddle.test/x")!, isGroupchat: false, to: state))
        #expect(!CallReactionLedger.belongs(reaction, from: ada, isGroupchat: true, to: state))
    }

    /// Each device in a room's call has a session id of its own: the room is what they share.
    @Test func inARoomsCallItIsTheRoomThatSaysWhichCall() {
        let state = CallState.inGroup(GroupCall(room: room, sid: "mine", media: audio, join: join, nick: "me"))
        let occupant = JID(parsing: "design@rooms.waddle.test/ada")!
        #expect(CallReactionLedger.belongs(InCallReaction(sid: "theirs", emoji: "👍"), from: occupant, isGroupchat: true, to: state))
        #expect(!CallReactionLedger.belongs(InCallReaction(sid: "mine", emoji: "👍"), from: JID(parsing: "other@rooms.waddle.test/ada")!, isGroupchat: true, to: state))
        #expect(!CallReactionLedger.belongs(InCallReaction(sid: "mine", emoji: "👍"), from: occupant, isGroupchat: false, to: state), "a private message from an occupant is not the room's")
        #expect(!CallReactionLedger.belongs(InCallReaction(sid: "mine", emoji: "👍"), from: JID(parsing: "design@rooms.waddle.test")!, isGroupchat: true, to: state), "the room itself reacts to nothing")
    }

    @Test func withNoCallThereIsNothingToReactTo() {
        let reaction = InCallReaction(sid: "s1", emoji: "👍")
        #expect(!CallReactionLedger.belongs(reaction, from: ada, isGroupchat: false, to: .idle))
        #expect(CallReactionLedger.destination(in: .idle) == nil)
        #expect(CallReactionLedger.destination(in: .ended(sid: "s1", reason: .missed)) == nil)
    }

    @Test func aReactionGoesToTheDeviceOrToTheRoom() {
        #expect(CallReactionLedger.destination(in: .active(ActiveCall(peer: ada, sid: "s1", media: audio, join: join))) == .device(ada, sid: "s1"))
        #expect(CallReactionLedger.destination(in: .inGroup(GroupCall(room: room, sid: "g1", media: audio, join: join, nick: "me"))) == .room(room, sid: "g1"))
    }
}

@MainActor @Suite("Call reactions in the store") struct CallReactionStoreTests {
    @Test func aReactionInARoomsCallGoesToTheRoomAndComesBackFromIt() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        #expect(await f.store.sendReaction("👍"))
        #expect(f.port.callSends.count == 1)
        guard case .reaction("👍", .room(groupRoom, _)) = f.port.callSends[0] else {
            Issue.record("expected a reaction to the room, got \(f.port.callSends)")
            return
        }
        #expect(f.store.reactions.bursts.isEmpty, "the room hands ours back with everyone else's")
        f.store.receiveReaction(InCallReaction(sid: "x", emoji: "👍"), from: nil, isGroupchat: true)
        #expect(f.store.reactions.bursts.isEmpty, "from nobody is from nobody in the call")
        f.store.receiveReaction(
            InCallReaction(sid: "someone-elses", emoji: "👍"), from: JID(parsing: "\(groupRoom)/alice")!, isGroupchat: true
        )
        #expect(f.store.reactions.bursts.map(\.emoji) == ["👍"])
        f.store.clear()
        #expect(f.store.reactions.bursts.isEmpty)
    }

    @Test func aReactionWithNoCallGoesNowhere() async {
        let f = GroupFixture()
        #expect(await f.store.sendReaction("👍") == false)
        #expect(await f.store.sendReaction("not an emoji") == false)
        #expect(f.port.callSends.isEmpty)
        f.store.receiveReaction(InCallReaction(sid: "s", emoji: "👍"), from: JID(parsing: "\(groupRoom)/alice")!, isGroupchat: true)
        #expect(f.store.reactions.bursts.isEmpty)
    }
}
