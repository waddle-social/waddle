import SwiftUI
import WaddleKit

/// The reactions a story has, each a toggle, then the quick ones nobody
/// has used yet.
struct StoryReactionBar: View {
    let reactions: StoryReactions
    let onToggle: (String) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Spacing.m) {
                ForEach(reactions.counts, id: \.emoji) { reaction in
                    let isMine = reactions.mine.contains(reaction.emoji)
                    Button { onToggle(reaction.emoji) } label: {
                        Text("\(reaction.emoji) \(reaction.count)")
                    }
                    .buttonStyle(.huddleChip(isSelected: isMine))
                    .accessibilityLabel(Text(CommunityCopy.reactionLabel(reaction, isMine: isMine)))
                    .accessibilityAddTraits(isMine ? .isSelected : [])
                }
                ForEach(unused, id: \.self) { emoji in
                    Button { onToggle(emoji) } label: {
                        Text(emoji)
                    }
                    .buttonStyle(.huddleChip())
                    .accessibilityLabel(Text("React with \(emoji)"))
                }
            }
            // Room for the chips' outlines, which a scroll view clips.
            .padding(Theme.Spacing.xxs)
        }
    }

    private var unused: [String] {
        let used = Set(reactions.counts.map(\.emoji))
        return Theme.quickReactions.filter { !used.contains($0) }
    }
}
