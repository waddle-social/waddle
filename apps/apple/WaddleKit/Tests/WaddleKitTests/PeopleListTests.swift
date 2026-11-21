import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The People view: everyone the account knows by address (those whose
/// presence it sees and those it has direct messages with), split into
/// who is here now and who is not.
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
        contacts: [BareJID: PresenceStore.ContactPresence] = [:],
        directPeers: [BareJID] = [],
        query: String = ""
    ) -> PeopleList {
        PeopleList.build(
            contacts: contacts, directPeers: directPeers, own: me.jid,
            name: { $0.description.split(separator: "@").first.map { $0.capitalized } ?? "" },
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

    @Test func someoneOnlyMessagedIsListedAsNotHere() {
        let people = list(contacts: [ada: presence(.available)], directPeers: [bo])

        #expect(people.here.map(\.jid) == [ada])
        #expect(people.away == [Person(jid: bo, name: "Bo", availability: .offline, status: nil)])
    }

    @Test func someoneBothSeenAndMessagedIsListedOnce() {
        let people = list(contacts: [ada: presence(.chat, "Around")], directPeers: [ada])

        #expect(people.here == [Person(jid: ada, name: "Ada", availability: .chat, status: "Around")])
        #expect(people.away.isEmpty)
    }

    /// The account's other devices send presence too; that is not a person
    /// to list.
    @Test func theAccountItselfIsNotListed() {
        let people = list(contacts: [me.jid: presence(.available), ada: presence(.available)], directPeers: [me.jid])

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
