/// One picked room link at its current scalar range in the draft. Keeping
/// the occurrence lets identical labels refer to different rooms, and
/// keeps other copies of the label plain.
public struct RoomLinkDraft: Hashable, Sendable {
    public let room: BareJID
    public var range: ComposerScalarRange

    public init(room: BareJID, range: ComposerScalarRange) {
        self.room = room
        self.range = range
    }
}

extension RoomLinks {
    /// Move records through an editor change. Native editors supply the
    /// old replacement range, which disambiguates repeated text.
    public static func relocate(
        _ links: [RoomLinkDraft], from oldText: String, to newText: String,
        replacing proposedRange: ComposerScalarRange?
    ) -> [RoomLinkDraft] {
        relocate(links, from: oldText, applying: ComposerTextReplacement(newText, replacing: proposedRange))
    }

    static func relocate(
        _ links: [RoomLinkDraft], from oldText: String, applying replacement: ComposerTextReplacement
    ) -> [RoomLinkDraft] {
        guard !links.isEmpty else { return [] }
        if case let .changes(_, changes) = replacement {
            return relocate(links, across: changes.map { ($0.range, $0.delta) })
        }
        let old = Array(oldText.unicodeScalars)
        let new = Array(replacement.text.unicodeScalars)
        let delta = new.count - old.count
        if case let .range(_, range) = replacement, range.lowerBound >= 0, range.upperBound <= old.count,
           range.count + delta >= 0,
           old[..<range.lowerBound].elementsEqual(new[..<range.lowerBound]),
           old[range.upperBound...].elementsEqual(new[(range.upperBound + delta)...]) {
            return relocate(links, across: [(range, delta)])
        }
        if old == new { return links }

        let difference = new.difference(from: old)
        let removed = offsets(in: difference.removals)
        let inserted = offsets(in: difference.insertions)
        let retained = zip(old.indices.filter { !removed.contains($0) }, new.indices.filter { !inserted.contains($0) })
        let mapping = Dictionary(uniqueKeysWithValues: retained)
        return links.compactMap { link in
            guard let start = mapping[link.range.lowerBound],
                  link.range.enumerated().allSatisfy({ mapping[$0.element] == start + $0.offset })
            else { return nil }
            return RoomLinkDraft(room: link.room, range: start..<(start + link.range.count))
        }
    }

    private static func relocate(
        _ links: [RoomLinkDraft], across changes: [(range: ComposerScalarRange, delta: Int)]
    ) -> [RoomLinkDraft] {
        links.compactMap { link in
            var shift = 0
            for change in changes {
                if change.range.upperBound <= link.range.lowerBound {
                    shift += change.delta
                } else if change.range.lowerBound >= link.range.upperBound {
                    break
                } else {
                    return nil
                }
            }
            return RoomLinkDraft(room: link.room, range: (link.range.lowerBound + shift)..<(link.range.upperBound + shift))
        }
    }

    private static func offsets(in changes: [CollectionDifference<Unicode.Scalar>.Change]) -> Set<Int> {
        Set(changes.map {
            switch $0 {
            case let .insert(offset, _, _), let .remove(offset, _, _): offset
            }
        })
    }
}
