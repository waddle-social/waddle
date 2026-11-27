extension DraftReferences {
    /// Picked labels are literal text while markdown around them still applies.
    func literalRanges(after fences: [Removal]) -> [ScalarRange] {
        roomLinks.compactMap { link in
            rebase(link.range, fences: fences, inline: []).map {
                ScalarRange(start: $0.lowerBound, end: $0.upperBound)
            }
        }
    }

    func rebased(fences: [Removal], inline: [Removal], spans: [MarkupSpan]) -> Self {
        let code = spans.filter { $0.kind == .code || $0.kind == .codeBlock }
        return Self(
            mentions: mentions.compactMap { mention in
                rebase(mention.range, fences: fences, inline: inline).map {
                    MentionDraft(target: mention.target, range: $0)
                }
            },
            roomLinks: roomLinks.compactMap { link in
                guard let range = rebase(link.range, fences: fences, inline: inline),
                      range.count == link.range.count,
                      !code.contains(where: { $0.start < range.upperBound && $0.end > range.lowerBound })
                else { return nil }
                return RoomLinkDraft(room: link.room, range: range)
            }
        )
    }
}
