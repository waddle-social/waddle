import Foundation
import Testing
@testable import WaddleKit

struct SlashTriggerTests {
    private func parse(_ text: String) -> (String, String)? {
        SlashTrigger.parse(text).map { ($0.prefix, $0.trailing) }
    }

    @Test func bareSlashHasEmptyPrefix() {
        #expect(parse("/")! == ("", ""))
        #expect(parse("/ hello")! == ("", "hello"))
    }

    @Test func commandWordAndTrailing() {
        #expect(parse("/shrug")! == ("shrug", ""))
        #expect(parse("/shrug ")! == ("shrug", ""))
        #expect(parse("/giphy   cats  ")! == ("giphy", "cats  "))
        #expect(parse("/My_cmd-2 x")! == ("My_cmd-2", "x"))
    }

    @Test func trailingSpansLines() {
        #expect(parse("/me waves\nand smiles")! == ("me", "waves\nand smiles"))
        #expect(parse("/me\n\n  waves")! == ("me", "waves"))
    }

    @Test func nonTriggersAreRejected() {
        #expect(parse("") == nil)
        #expect(parse("hello /me") == nil)
        #expect(parse(" /me waves") == nil)
        #expect(parse("/1abc") == nil)
        #expect(parse("/-x") == nil)
        #expect(parse("/foo!") == nil)
        #expect(parse("/foo/bar") == nil)
        #expect(parse("/café") == nil)
    }
}
