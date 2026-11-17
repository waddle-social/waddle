import Testing

@testable import WaddleKit

struct TieBreakTests {
    @Test func typedPeerComparisonIgnoresCaseAndResource() throws {
        let phone = try #require(JID(parsing: "Bob@Waddle.Test/phone"))
        let desk = try #require(JID(parsing: "bob@waddle.test/desk"))
        let bare = try #require(JID(parsing: "bob@waddle.test"))
        let other = try #require(JID(parsing: "eve@waddle.test"))
        #expect(phone.bare == desk.bare)
        #expect(bare.bare == desk.bare)
        #expect(bare.bare != other.bare)
    }

    @Test func canonicalUnicodeEquivalenceDoesNotCollapseDistinctOctets() {
        // Precomposed é is C3 A9; decomposed e + accent is 65 CC 81.
        #expect(compareOctetStrings("é", "e\u{301}") > 0)
        #expect(compareOctetStrings("e\u{301}", "é") < 0)
    }

    @Test func equalSIDsUseFullJIDOctetOrder() throws {
        #expect(
            incomingProposeWinsTieBreak(
                incomingSid: "c1", outgoingSid: "c1",
                incomingFrom: try #require(JID(parsing: "alice@waddle.test/phone")),
                ourFullJID: try #require(JID(parsing: "bob@waddle.test/desk"))
            ))
        #expect(
            !incomingProposeWinsTieBreak(
                incomingSid: "c1", outgoingSid: "c1",
                incomingFrom: try #require(JID(parsing: "bob@waddle.test/desk")),
                ourFullJID: try #require(JID(parsing: "alice@waddle.test/phone"))
            ))
    }

    @Test func higherIncomingSIDLoses() throws {
        #expect(
            !incomingProposeWinsTieBreak(
                incomingSid: "c1bbb", outgoingSid: "c1aaa",
                incomingFrom: try #require(JID(parsing: "bob@waddle.test/phone")),
                ourFullJID: try #require(JID(parsing: "alice@waddle.test/desk"))
            ))
    }

    @Test func digitsSortBeforeLowercaseLetters() {
        #expect(compareOctetStrings("c1", "ca") < 0)
    }

    @Test func octetComparisonUsesUnsignedUTF8() {
        // `é` starts with 0xC3, above ASCII `z` (0x7A).
        #expect(compareOctetStrings("z", "é") < 0)
        #expect(compareOctetStrings("é", "z") > 0)
    }

    @Test func sharedPrefixOrdersShorterFirst() {
        #expect(compareOctetStrings("ab", "abc") < 0)
        #expect(compareOctetStrings("abc", "ab") > 0)
    }

    @Test func octetComparisonOrdersASCII() {
        #expect(compareOctetStrings("abc", "abd") < 0)
        #expect(compareOctetStrings("abd", "abc") > 0)
        #expect(compareOctetStrings("abc", "abc") == 0)
    }

    @Test func lowerIncomingSIDWins() throws {
        #expect(
            incomingProposeWinsTieBreak(
                incomingSid: "c1aaa", outgoingSid: "c1bbb",
                incomingFrom: try #require(JID(parsing: "bob@waddle.test/phone")),
                ourFullJID: try #require(JID(parsing: "alice@waddle.test/desk"))
            ))
    }
}
