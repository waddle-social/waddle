import SwiftUI
import WaddleKit

/// The composer popover for any kind of completion.
struct ComposerSuggestionList: View {
    private static let maxHeight: CGFloat = 224

    let items: [ComposerSuggestion]
    let selectedIndex: Int
    let onPick: (ComposerSuggestion) -> Void

    var body: some View {
        let contents = items.map(SuggestionRowContent.init)
        let selected = contents[selectedIndex]
        HeightUpTo(limit: Self.maxHeight) {
            ViewThatFits(in: .vertical) {
                rows(contents).fixedSize(horizontal: false, vertical: true)
                ScrollViewReader { proxy in
                    ScrollView { rows(contents) }
                        .onChange(of: selectedIndex, initial: true) { _, index in
                            proxy.scrollTo(index)
                        }
                }
            }
        }
        .huddleSurface(.raised, in: RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(selected.listAccessibilityTitle))
        .onChange(of: items[selectedIndex], initial: true) { _, _ in
            AccessibilityNotification.Announcement(selected.accessibilityTitle).post()
        }
    }

    private func rows(_ contents: [SuggestionRowContent]) -> some View {
        VStack(spacing: Theme.Spacing.xxs) {
            ForEach(items.indices, id: \.self) { index in
                ComposerSuggestionRow(row: contents[index], isSelected: index == selectedIndex) {
                    onPick(items[index])
                }
                .id(index)
            }
        }
        .padding(WaddleTokens.Spacing.s1Half)
    }
}

private struct ComposerSuggestionRow: View {
    let row: SuggestionRowContent
    let isSelected: Bool
    let onPick: () -> Void
    @State private var isHovered = false

    var body: some View {
        Button(action: onPick) {
            HStack(spacing: Theme.Spacing.s) {
                leading
                if let title = row.title {
                    Text(title)
                        .huddleText(Theme.Typography.label.weight(700))
                        .layoutPriority(1)
                }
                if let detail = row.detail {
                    Text(detail)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    Spacer(minLength: 0)
                }
                if row.isBuiltin {
                    HuddleTag("Built-in")
                        .fixedSize()
                }
            }
            .lineLimit(1)
            .padding(.horizontal, Theme.Spacing.m)
            .frame(minHeight: rowHeight)
            .background(background, in: RoundedRectangle(cornerRadius: Theme.Radius.medium))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .accessibilityLabel(Text(row.accessibilityTitle))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var rowHeight: CGFloat {
        #if os(iOS)
        Theme.Size.minHitTarget
        #else
        WaddleTokens.ControlHeight.md
        #endif
    }

    private var background: Color {
        if isSelected { return Theme.State.selected }
        return isHovered ? Theme.State.hover : .clear
    }

    @ViewBuilder
    private var leading: some View {
        switch row.leading {
        case let .avatar(jid, name):
            JIDAvatar(jid: jid, name: name, size: Theme.Size.rowGlyph)
                .accessibilityHidden(true)
        case let .glyph(glyph):
            Text(glyph)
                .frame(width: Theme.Size.rowGlyph)
                .accessibilityHidden(true)
        case let .symbol(symbol):
            Image(systemName: symbol)
                .frame(width: Theme.Size.rowGlyph, height: Theme.Size.rowGlyph)
                .foregroundStyle(Theme.Colors.actionText)
                .accessibilityHidden(true)
        }
    }
}

private struct SuggestionRowContent {
    enum Leading {
        case avatar(BareJID, name: String)
        case glyph(String)
        case symbol(String)
    }

    let leading: Leading
    let title: String?
    let detail: String?
    let isBuiltin: Bool
    let accessibilityTitle: String
    let listAccessibilityTitle: String

    init(_ item: ComposerSuggestion) {
        switch item {
        case let .mention(candidate):
            leading = candidate.jid.map { .avatar($0, name: candidate.name) }
                ?? .symbol(candidate.target == .here ? "dot.radiowaves.left.and.right" : "megaphone")
            title = candidate.name
            detail = nil
            isBuiltin = false
            accessibilityTitle = candidate.token
            listAccessibilityTitle = "Mention suggestions"
        case let .room(room):
            leading = .symbol("number")
            title = room.label
            detail = room.addressDetail
            isBuiltin = false
            accessibilityTitle = room.accessibilityName
            listAccessibilityTitle = "Link a room"
        case let .emoji(candidate):
            leading = .glyph(candidate.glyph)
            title = nil
            detail = ":\(candidate.name):"
            isBuiltin = false
            accessibilityTitle = candidate.name.replacingOccurrences(of: "_", with: " ")
            listAccessibilityTitle = "Emoji suggestions"
        case let .slash(candidate):
            leading = .symbol("slash.circle")
            title = candidate.usage
            detail = candidate.description
            if case .builtin = candidate { isBuiltin = true } else { isBuiltin = false }
            accessibilityTitle = "\(candidate.usage), \(candidate.description)"
            listAccessibilityTitle = "Command suggestions"
        }
    }
}
