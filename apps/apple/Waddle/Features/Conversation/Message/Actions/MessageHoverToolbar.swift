import SwiftUI
import WaddleKit

/// A message row's hover toolbar where it floats: over the row's top
/// trailing corner, reaching up over the row above as on web, so it never
/// moves the layout. The only view that reads the shared hover state, so
/// a pointer crossing redraws two rows' toolbars, not every row.
struct MessageRowToolbar: View {
    @Environment(MessageActionModel.self) private var actions
    let item: TimelineItem
    let context: MessageActionContext
    let isRowHovered: Bool
    let perform: (MessageAction) -> Void

    var body: some View {
        let row = item.presentationID
        if actions.showsToolbar(of: row, isRowHovered: isRowHovered) {
            GeometryReader { geometry in
                let buttons = MessageActions.toolbar(
                    for: item,
                    in: context,
                    isBeingEdited: actions.composer.editing?.presentationID == row,
                    maxButtons: MessageHoverToolbar.buttonsFitting(geometry.size.width - 2 * Theme.Spacing.l)
                )
                if !buttons.isEmpty {
                    MessageHoverToolbar(actions: buttons, perform: perform)
                        .onHover { inside in
                            if inside {
                                actions.holdToolbar(of: row)
                            } else {
                                actions.releaseToolbar(of: row)
                            }
                        }
                        .onDisappear { actions.releaseToolbar(of: row) }
                        .padding(.trailing, Theme.Spacing.l)
                        .offset(y: -Theme.Spacing.l)
                        .frame(maxWidth: .infinity, alignment: .topTrailing)
                }
            }
        }
    }
}

/// The toolbar (web `MessageHoverToolbar`): quick reactions, add
/// reaction, reply, the thread, pin, then edit and delete for one's own
/// message, each with its tooltip.
struct MessageHoverToolbar: View {
    let actions: [MessageAction]
    let perform: (MessageAction) -> Void

    private static let spacing = Theme.Spacing.xxs

    /// How many buttons fit in `width`, the divider before one's own
    /// actions included.
    static func buttonsFitting(_ width: CGFloat) -> Int {
        let side = HuddleToolbarButtonStyle.side
        let usable = width - 2 * spacing - 2 * WaddleTokens.BorderWidth.hair - (side / 4)
        return max(0, Int((usable + spacing) / (side + spacing)))
    }

    var body: some View {
        HStack(spacing: Self.spacing) {
            ForEach(Array(actions.enumerated()), id: \.element) { index, action in
                if action.isOwn, index > 0, !actions[index - 1].isOwn {
                    Rectangle()
                        .fill(Theme.Colors.border)
                        .frame(width: WaddleTokens.BorderWidth.hair, height: Theme.Spacing.l + Theme.Spacing.xs)
                        .padding(.horizontal, Theme.Spacing.xxs)
                        .accessibilityHidden(true)
                }
                Button {
                    perform(action)
                } label: {
                    if case let .react(emoji) = action {
                        Text(emoji).huddleText(Theme.Typography.body)
                    } else {
                        Image(systemName: action.symbol).huddleText(Theme.Typography.label)
                    }
                }
                .buttonStyle(HuddleToolbarButtonStyle(isDanger: action == .delete))
                .help(action.title)
                .accessibilityLabel(Text(action.accessibilityLabel))
            }
        }
        .padding(Self.spacing)
        .huddleSurface(.raised, radius: Theme.Radius.medium)
    }
}
