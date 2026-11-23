import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The People view: everyone the account knows by address (its contacts,
/// anyone else whose presence it sees and those it has direct messages
/// with), split into who is here now and who is not.
@Suite("People list")
struct PeopleListTests {
    private let ada = bare("ada@waddle.test")
    private let bo = bare("bo@waddle.test")
    private let cy = bare("cy@waddle.test")
    private let di = bare("di@waddle.test")

    private func presence(_ availability: Availability, _ status: String? = nil) -> PresenceStore.ContactPresence {
        PresenceStore.ContactPresence(availability: availability, status: status, idleSince: nil)
    }

    private func list(
        saved: [Contact] = [],
        contacts: [BareJID: PresenceStore.ContactPresence] = [:],
        directPeers: [BareJID] = [],
        inCall: Set<BareJID> = [],
        query: String = ""
    ) -> PeopleList {
        PeopleList.build(
            contacts: saved, presence: contacts, directPeers: directPeers, own: me.jid,
            name: { $0.description.split(separator: "@").first.map { $0.capitalized } ?? "" },
            isInCall: inCall.contains,
            query: query
        )
    }

    /// The same rule as the web app: available or do-not-disturb is here;
    /// away and offline are not.
    @Test func peopleAreSplitIntoHereAndNotHere() {
        let people = list(contacts: [
            ada: presence(.available), bo: presence(.doNotDisturb), cy: presence(.away), di: presence(.offline),
        ])

        #expect(people.here.map(\.jid) == [ada, bo])
        #expect(people.away.map(\.jid) == [cy, di])
    }

    /// Someone offline sends no presence at all. They are still a contact,
    /// and are listed from the contact list.
    @Test func aContactWhoIsOfflineIsStillListed() {
        let people = list(saved: [Contact(jid: ada), Contact(jid: bo)], contacts: [ada: presence(.available)])

        #expect(people.here.map(\.jid) == [ada])
        #expect(people.away == [Person(jid: bo, name: "Bo", availability: .offline, status: nil, isInCall: false)])
    }

    @Test func aContactIsShownByTheNameSavedForThem() {
        let people = list(saved: [Contact(jid: ada, name: "Ada Lovelace"), Contact(jid: bo, name: nil)])

        #expect(people.away.map(\.name) == ["Ada Lovelace", "Bo"])
        #expect(list(saved: [Contact(jid: ada, name: "Ada Lovelace")], query: "lovelace").away.map(\.jid) == [ada])
    }

    /// The row on Home counts people here without building the list.
    @Test func theCountOfPeopleHereMatchesTheList() {
        let contacts = [
            me.jid: presence(.available), ada: presence(.available), bo: presence(.doNotDisturb),
            cy: presence(.away),
        ]

        #expect(PeopleList.hereCount(presence: contacts, own: me.jid) == 2)
        #expect(list(contacts: contacts).here.count == 2)
    }

    @Test func someoneOnlyMessagedIsListedAsNotHere() {
        let people = list(contacts: [ada: presence(.available)], directPeers: [bo])

        #expect(people.here.map(\.jid) == [ada])
        #expect(people.away == [Person(jid: bo, name: "Bo", availability: .offline, status: nil, isInCall: false)])
    }

    @Test func someoneBothSeenAndMessagedIsListedOnce() {
        let people = list(contacts: [ada: presence(.chat, "Around")], directPeers: [ada])

        #expect(people.here == [Person(jid: ada, name: "Ada", availability: .chat, status: "Around", isInCall: false)])
        #expect(people.away.isEmpty)
    }

    /// Being in a call marks someone; it does not move them in the list.
    @Test func someoneInACallIsMarkedWhereTheyAreListed() {
        let people = list(contacts: [ada: presence(.available), bo: presence(.doNotDisturb)], inCall: [bo])

        #expect(people.here.map(\.jid) == [ada, bo])
        #expect(people.here.map(\.isInCall) == [false, true])
    }

    /// The account's other devices send presence too; that is not a person
    /// to list.
    @Test func theAccountItselfIsNotListed() {
        let people = list(
            saved: [Contact(jid: me.jid)], contacts: [me.jid: presence(.available), ada: presence(.available)],
            directPeers: [me.jid]
        )

        #expect(people.here.map(\.jid) == [ada])
        #expect(people.away.isEmpty)
    }

    @Test func eachGroupIsOrderedByAvailabilityThenName() {
        let people = list(contacts: [
            di: presence(.doNotDisturb), cy: presence(.available), ada: presence(.available),
            bo: presence(.extendedAway),
        ], directPeers: [bare("zed@waddle.test"), bare("eve@waddle.test")])

        #expect(people.here.map(\.name) == ["Ada", "Cy", "Di"])
        #expect(people.away.map(\.name) == ["Bo", "Eve", "Zed"])
    }

    @Test func aSearchKeepsPeopleWhoseNameOrAddressMatches() {
        let contacts = [ada: presence(.available), bo: presence(.away)]

        #expect(list(contacts: contacts, query: " AD ").here.map(\.jid) == [ada])
        #expect(list(contacts: contacts, query: "ada").away.isEmpty)
        #expect(list(contacts: contacts, query: "bo@waddle").away.map(\.jid) == [bo])
        #expect(list(contacts: contacts, query: "nobody").isEmpty)
    }
}
