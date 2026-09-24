import Foundation
import Testing
@testable import WaddleKit

struct ExtensionCommandPrefillTests {
    private func field(
        _ variable: String,
        _ type: ExtensionFieldType = .textSingle,
        required: Bool = true,
        blocked: Bool = false,
        values: [String] = []
    ) -> ExtensionCommandField {
        ExtensionCommandField(
            variable: variable,
            label: nil,
            type: type,
            required: required,
            blocked: blocked,
            options: [],
            values: values
        )
    }

    private func form(_ fields: [ExtensionCommandField]) -> ExtensionCommandForm {
        ExtensionCommandForm(title: nil, instructions: nil, fields: fields)
    }

    @Test func fillsTheFirstEmptyRequiredTextField() {
        let filled = form([
            field("intro", .fixed, required: false),
            field("note", required: false),
            field("done", required: true, values: ["x"]),
            field("question"),
            field("other"),
        ]).prefillingFirstRequired(with: "Lunch?")
        #expect(filled.field("question")?.values == ["Lunch?"])
        #expect(filled.field("other")?.values == [])
        #expect(filled.field("note")?.values == [])
        #expect(filled.field("done")?.values == ["x"])
    }

    @Test func skipsSecretsListsAndBooleans() {
        let filled = form([
            field("token", .textPrivate, blocked: true),
            field("secret", blocked: true),
            field("choice", .listSingle),
            field("flag", .boolean),
            field("who", .jidSingle),
        ]).prefillingFirstRequired(with: "alice@waddle.test")
        #expect(filled.fields.map(\.values) == [[], [], [], [], ["alice@waddle.test"]])
    }

    @Test func unchangedWhenNothingFits() {
        let original = form([field("flag", .boolean), field("optional", required: false)])
        #expect(original.prefillingFirstRequired(with: "x") == original)
    }
}
