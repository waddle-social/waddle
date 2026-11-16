import Testing
import WaddleKit

@Suite("Group call roster") struct GroupCallRosterTests {
    private let own = BareJID(parsing: "me@waddle.test")!
    private let audio = CallMedia(audio: true, video: false)

    private func jid(_ raw: String) -> JID { JID(parsing: raw)! }
    private func bare(_ raw: String) -> BareJID { BareJID(parsing: raw)! }

    private func participant(_ nick: String, _ realJID: String?, hand: Bool = false, muted: Bool = false) -> GroupCallParticipant {
        GroupCallParticipant(nick: nick, realJID: realJID.map(jid), media: audio, isHandRaised: hand, isMuted: muted)
    }

    private func rows(
        presence: [GroupCallParticipant] = [], live: [String] = [], speaking: Set<BareJID> = []
    ) -> [GroupCallRosterRow] {
        GroupCallRoster.rows(presence: presence, live: live.map(jid), speaking: speaking, own: own)
    }

    // MARK: - Presence fallback

    @Test func withoutLiveParticipantsThePresenceRosterIsShownInOrder() {
        let roster = rows(presence: [participant("alice", "alice@waddle.test/web"), participant("bob", "bob@waddle.test/phone")])
        #expect(roster == [
            GroupCallRosterRow(nick: "alice", jid: bare("alice@waddle.test")),
            GroupCallRosterRow(nick: "bob", jid: bare("bob@waddle.test")),
        ])
    }

    @Test func presenceRowsCarryTheirHandAndMuteBadges() {
        let roster = rows(presence: [participant("bob", nil, hand: true), participant("carol", nil, muted: true)])
        #expect(roster == [
            GroupCallRosterRow(nick: "bob", jid: nil, isHandRaised: true),
            GroupCallRosterRow(nick: "carol", jid: nil, isMuted: true),
        ])
    }

    @Test func aPresenceRowWithoutARealJIDStaysUnresolved() {
        #expect(rows(presence: [participant("carol", nil)]) == [GroupCallRosterRow(nick: "carol", jid: nil)])
    }

    // MARK: - Live participants

    @Test func liveParticipantsReplaceAStalePresenceRoster() {
        let roster = rows(
            presence: [participant("stale", nil), participant("alice", "alice@waddle.test/web")],
            live: ["alice@waddle.test/web"])
        #expect(roster == [GroupCallRosterRow(nick: "alice", jid: bare("alice@waddle.test"))])
    }

    @Test func aLiveIdentityWithoutAnOwnerIsLabelledByItsLocalpart() {
        #expect(rows(live: ["carol@waddle.test/web"]) == [GroupCallRosterRow(nick: "carol", jid: bare("carol@waddle.test"))])
    }

    @Test func anOwnerMatchesTheIdentityCaseInsensitivelyInItsBarePart() {
        let roster = rows(presence: [participant("ally", "Alice@Waddle.Test/Web")], live: ["alice@waddle.test/Web"])
        #expect(roster == [GroupCallRosterRow(nick: "ally", jid: bare("alice@waddle.test"))])
    }

    @Test func anOwnerWhoseResourceDiffersInCaseDoesNotMatch() {
        let roster = rows(presence: [participant("ally", "alice@waddle.test/WEB")], live: ["alice@waddle.test/web"])
        #expect(roster == [GroupCallRosterRow(nick: "alice", jid: bare("alice@waddle.test"))])
    }

    @Test func badgesComeFromTheOwnerWhoseRealJIDIsTheIdentity() {
        let roster = rows(presence: [participant("ally", "alice@x.test/phone", hand: true, muted: true)], live: ["alice@x.test/phone"])
        #expect(roster == [GroupCallRosterRow(nick: "ally", jid: bare("alice@x.test"), isHandRaised: true, isMuted: true)])
    }

    @Test func anOwnedIdentityNeverClaimsAnotherAccountsRowByLocalpart() {
        for live in [["alice@y.test/phone", "alice@x.test/web"], ["alice@x.test/web", "alice@y.test/phone"]] {
            let roster = rows(presence: [participant("ally", "alice@y.test/phone")], live: live)
            #expect(Set(roster) == [
                GroupCallRosterRow(nick: "ally", jid: bare("alice@y.test")),
                GroupCallRosterRow(nick: "alice", jid: bare("alice@x.test")),
            ])
        }
    }

    /// The nick "alice" belongs to Bob; Alice, unowned, is also labelled
    /// "alice" by her localpart. Neither row borrows the other's face or badge.
    @Test func liveRowsTakeLabelFaceAndBadgesFromTheSameIdentity() {
        let roster = rows(presence: [participant("alice", "bob@y.test/phone", hand: true)], live: ["alice@x.test/web", "bob@y.test/phone"])
        #expect(roster == [
            GroupCallRosterRow(nick: "alice", jid: bare("alice@x.test")),
            GroupCallRosterRow(nick: "alice", jid: bare("bob@y.test"), isHandRaised: true),
        ])
    }

    @Test func anOwnerWhoIsNotLiveNeverRelabelsSomeoneElse() {
        let roster = rows(presence: [participant("alice", "bob@y.test/phone", hand: true)], live: ["alice@x.test/web"])
        #expect(roster == [GroupCallRosterRow(nick: "alice", jid: bare("alice@x.test"))])
    }

    @Test func aSecondDeviceTakesTheOwnerNickOfItsAccount() {
        let identities = ["alice@x.test/web", "alice@x.test/phone", "carol@x.test/web"]
        for live in [identities, identities.reversed()] {
            let roster = rows(presence: [participant("ally", "alice@x.test/phone", hand: true)], live: live)
            #expect(Set(roster) == [
                GroupCallRosterRow(nick: "ally", jid: bare("alice@x.test"), isHandRaised: true),
                GroupCallRosterRow(nick: "carol", jid: bare("carol@x.test")),
            ])
        }
    }

    @Test func onePersonUnderTwoNicksIsOneRow() {
        let presence = [
            participant("ally", "alice@x.test/web", muted: true), participant("bob", "bob@x.test/web"),
            participant("alice", "alice@x.test/phone", hand: true),
        ]
        let merged = [
            GroupCallRosterRow(nick: "ally", jid: bare("alice@x.test"), isHandRaised: true),
            GroupCallRosterRow(nick: "bob", jid: bare("bob@x.test")),
        ]
        // A hand on either device is up; a person is muted only if every device is.
        #expect(rows(presence: presence) == merged)
        #expect(rows(presence: presence, live: ["alice@x.test/web", "bob@x.test/web", "alice@x.test/phone"]) == merged)
    }

    @Test func twoPeopleWhoseAccountsAreUnknownStaySeparate() {
        #expect(rows(presence: [participant("ally", nil), participant("alice", nil)]).count == 2)
    }

    @Test func onePersonOnTwoDevicesIsOneRowKeepingTheFirstPosition() {
        let roster = rows(live: ["alice@x.test/web", "carol@x.test/web", "alice@x.test/phone"])
        #expect(roster == [
            GroupCallRosterRow(nick: "alice", jid: bare("alice@x.test")),
            GroupCallRosterRow(nick: "carol", jid: bare("carol@x.test")),
        ])
    }

    @Test func aDomainIdentityIsLabelledByItsDomainAndHasNoFace() {
        #expect(rows(live: ["bridge.x.test/bot"]) == [GroupCallRosterRow(nick: "bridge.x.test", jid: nil)])
    }

    // MARK: - Voices and self

    @Test func aRowWhoseAccountIsSpeakingIsMarkedSpeaking() {
        let roster = rows(live: ["alice@x.test/web", "carol@x.test/web"], speaking: [bare("carol@x.test")])
        #expect(roster == [
            GroupCallRosterRow(nick: "alice", jid: bare("alice@x.test")),
            GroupCallRosterRow(nick: "carol", jid: bare("carol@x.test"), isSpeaking: true),
        ])
    }

    @Test func aRowWithoutAJIDIsNeverSpeaking() {
        #expect(rows(presence: [participant("carol", nil)], speaking: [bare("carol@waddle.test")])
            == [GroupCallRosterRow(nick: "carol", jid: nil)])
    }

    @Test func ourOwnRowIsMarkedAndMovedToTheFrontWithOthersKeepingTheirOrder() {
        let roster = rows(live: ["alice@x.test/web", "bob@x.test/web", "me@waddle.test/phone", "carol@x.test/web"])
        #expect(roster == [
            GroupCallRosterRow(nick: "me", jid: own, isSelf: true),
            GroupCallRosterRow(nick: "alice", jid: bare("alice@x.test")),
            GroupCallRosterRow(nick: "bob", jid: bare("bob@x.test")),
            GroupCallRosterRow(nick: "carol", jid: bare("carol@x.test")),
        ])
    }

    @Test func ourOwnPresenceRowIsMarkedToo() {
        let roster = rows(presence: [participant("bob", "bob@x.test/web"), participant("me", "me@waddle.test/phone", muted: true)])
        #expect(roster == [
            GroupCallRosterRow(nick: "me", jid: own, isMuted: true, isSelf: true),
            GroupCallRosterRow(nick: "bob", jid: bare("bob@x.test")),
        ])
    }

    // MARK: - Speakers

    @Test func speakersAreTheAccountsOfTheirIdentities() {
        #expect(GroupCallRoster.speakers(["alice@x.test/web", "Alice@X.test/phone", "me@waddle.test/mac"])
            == [bare("alice@x.test"), own])
    }

    @Test func aSpeakerIdentityThatIsNotAnAccountIsDropped() {
        #expect(GroupCallRoster.speakers(["bridge.x.test/bot", "", "not a jid@"]) == [])
    }
}
