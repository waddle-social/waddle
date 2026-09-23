import Foundation

/// A row turned back into composer input for a XEP-0308 edit: markdown
/// that re-creates its XEP-0394 spans, and its XEP-0372 mentions over that
/// markdown, so an unchanged edit re-sends the same markup and references.
public struct EditableMessage: Hashable, Sendable {
    public let text: String
    /// Ranges over `text`, in Unicode scalars. References that do not map
    /// cleanly onto `text` are dropped and stay plain text.
    public let mentions: [MentionDraft]

    public init(item: TimelineItem) {
        let scalars = Array(item.body.unicodeScalars)
        guard let mapping = WireBodyMapping(item: item) else {
            self.text = item.body
            self.mentions = []
            return
        }
        let markup = EditableMarkup(spans: item.message.markupSpans, mapping: mapping, displayedLength: scalars.count)
        let text = markup.apply(to: scalars)
        self.text = text
        self.mentions = Self.mentions(item.message.references, mapping: mapping, markup: markup, in: text)
    }

    /// The composer's picked-mention records, re-located on every send the
    /// same way as mentions picked for a new message.
    public var recordedMentions: [RecordedMention] {
        let scalars = Array(text.unicodeScalars)
        return mentions.map { RecordedMention(token: Self.string(scalars[$0.range]), target: $0.target) }
    }

    /// Keeps only mentions whose text reads as an `@` token that the send
    /// path re-locates at the same place, so what is shown as a mention is
    /// exactly what an unchanged edit sends.
    private static func mentions(
        _ references: [Reference],
        mapping: WireBodyMapping,
        markup: EditableMarkup,
        in text: String
    ) -> [MentionDraft] {
        let scalars = Array(text.unicodeScalars)
        let candidates = references.compactMap { reference -> (MentionDraft, RecordedMention)? in
            guard let target = MentionTarget(reference: reference),
                  let displayed = mapping.displayedRange(ofWire: reference.begin, reference.end),
                  let range = markup.editableRange(ofDisplayed: displayed),
                  range.upperBound <= scalars.count,
                  scalars[range.lowerBound] == "@"
            else { return nil }
            let token = string(scalars[range])
            return (MentionDraft(target: target, range: range), RecordedMention(token: token, target: target))
        }
        let located = Set(MentionTokens.locate(candidates.map(\.1), in: text))
        return Array(located.intersection(candidates.map(\.0)))
            .sorted { ($0.range.lowerBound, $0.range.upperBound) < ($1.range.lowerBound, $1.range.upperBound) }
    }

    private static func string(_ scalars: ArraySlice<Unicode.Scalar>) -> String {
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars)
        return String(view)
    }
}
