import SwiftUI
import WaddleKit

/// XEP-0444 reaction pills plus an add-reaction button.
struct MessageReactionsBar: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @State private var showsPicker = false
    let item: TimelineItem

    var body: some View {
        let canReact = item.actionTargetID != nil
        ReactionFlowLayout(spacing: Theme.Spacing.xs + 2) {
            ForEach(item.reactions) { reaction in
                MessageReactionPill(reaction: reaction) {
                    actions.react(reaction.emoji, to: item, session: session)
                }
                .disabled(!canReact)
            }
            addButton
                .disabled(!canReact)
        }
    }

    private var addButton: some View {
        Button {
            showsPicker = true
        } label: {
            Image(systemName: "face.smiling")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.horizontal, Theme.Spacing.s)
                .padding(.vertical, 5)
                .background(Capsule().fill(Color.secondary.opacity(0.1)))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help("Add reaction")
        .accessibilityLabel(Text("Add reaction"))
        .popover(isPresented: $showsPicker) {
            MessageEmojiPicker { emoji in
                showsPicker = false
                actions.react(emoji, to: item, session: session)
            }
            .presentationCompactAdaptation(.popover)
        }
    }
}

/// One emoji with its count; highlighted when the account reacted.
struct MessageReactionPill: View {
    let reaction: ReactionGroup
    let onToggle: () -> Void

    var body: some View {
        Button(action: onToggle) {
            HStack(spacing: Theme.Spacing.xs) {
                Text(reaction.emoji)
                Text("\(reaction.count)")
                    .font(.caption.weight(.semibold).monospacedDigit())
                    .foregroundStyle(reaction.includesMine ? Color.accentColor : Color.secondary)
            }
            .padding(.horizontal, Theme.Spacing.s)
            .padding(.vertical, 3)
            .background(Capsule().fill(reaction.includesMine ? Color.accentColor.opacity(0.16) : Color.secondary.opacity(0.1)))
            .overlay(Capsule().strokeBorder(reaction.includesMine ? Color.accentColor.opacity(0.55) : Color.clear, lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help(reaction.reactors.joined(separator: ", "))
        .accessibilityLabel(Text(MessageAccessibilityText.label(for: reaction)))
        .accessibilityAddTraits(reaction.includesMine ? .isSelected : [])
        .accessibilityHint(Text(reaction.includesMine ? "Removes your reaction" : "Adds this reaction"))
    }
}

/// Wraps pills onto new lines when they do not fit.
struct ReactionFlowLayout: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(subviews, maxWidth: proposal.width ?? .infinity)
        let width = rows.map(\.width).max() ?? 0
        let height = rows.reduce(0) { $0 + $1.height } + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in arrange(subviews, maxWidth: bounds.width) {
            var x = bounds.minX
            for index in row.indices {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y), anchor: .topLeading, proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row {
        var indices: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func arrange(_ subviews: Subviews, maxWidth: CGFloat) -> [Row] {
        var rows: [Row] = []
        var current = Row()
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(.unspecified)
            let proposedWidth = current.indices.isEmpty ? size.width : current.width + spacing + size.width
            if !current.indices.isEmpty, proposedWidth > maxWidth {
                rows.append(current)
                current = Row()
            }
            current.width = current.indices.isEmpty ? size.width : current.width + spacing + size.width
            current.height = max(current.height, size.height)
            current.indices.append(index)
        }
        if !current.indices.isEmpty {
            rows.append(current)
        }
        return rows
    }
}
