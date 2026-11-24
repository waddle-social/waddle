import SwiftUI
import WaddleKit

/// The strip over a stage's tiles while we share our screen (web
/// `CallSelfShareNotice`): a red dot, the warning, and what everyone sees
/// as a thumbnail. Our own screen is never a tile on our own stage.
struct CallSelfShareNotice: View {
    /// Our shared screen, as the tile source knows it.
    let screen: CallStageTile
    let source: CallTileSource

    var body: some View {
        HStack(spacing: WaddleTokens.Spacing.s2Half) {
            Circle()
                .fill(Theme.Colors.danger)
                .frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                .accessibilityHidden(true)
            Text("You’re sharing your screen")
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                .foregroundStyle(Theme.Colors.fg)
            Spacer(minLength: Theme.Spacing.s)
            // Compared by value, as a tile's picture is: the stage redraws
            // whenever someone speaks and the screen's view must not.
            CallSelfShareThumbnail(screen: screen, source: source)
                .equatable()
                .frame(width: Metrics.thumbnail.width, height: Metrics.thumbnail.height)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous))
                .accessibilityHidden(true)
        }
        .padding(.leading, Theme.Spacing.m)
        .padding([.trailing, .vertical], Theme.Spacing.s)
        .background(Metrics.shape.fill(Theme.Colors.danger.opacity(0.14)))
        .overlay(Metrics.shape.strokeBorder(Theme.Colors.danger.opacity(0.4), lineWidth: WaddleTokens.BorderWidth.hair))
        .accessibilityElement(children: .combine)
    }

    private enum Metrics {
        static let shape = RoundedRectangle(cornerRadius: 12, style: .continuous)
        static let thumbnail = CGSize(width: 96, height: 54)
    }
}

private struct CallSelfShareThumbnail: View, Equatable {
    let screen: CallStageTile
    let source: CallTileSource

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.screen.id == rhs.screen.id && lhs.source === rhs.source
    }

    var body: some View {
        ZStack {
            Theme.Colors.surface
            source.video(screen)
        }
    }
}

/// Picks what to share: a whole screen, or one window (web: the browser's own picker).
///
/// Pure: the list is passed in.
struct CallScreenPicker: View {
    let sources: CallScreenSources
    let pick: @MainActor (CallScreenSource) -> Void
    let cancel: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Share your screen")
                .huddleText(Theme.Typography.subtitle.sized(WaddleTokens.FontSize.md))
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
                .padding([.horizontal, .top], Theme.Spacing.l)
            Text("Everyone in the call sees what you pick, until you stop sharing.")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .padding(.horizontal, Theme.Spacing.l)
                .padding(.top, Theme.Spacing.xs)
                .padding(.bottom, Theme.Spacing.m)
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    if sources.isEmpty {
                        Text("There is nothing to share right now.")
                            .huddleText(Theme.Typography.body.sized(WaddleTokens.FontSize.sm))
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .padding(Theme.Spacing.m)
                    }
                    section("Screens", sources.screens, symbol: "display")
                    section("Windows", sources.windows, symbol: "macwindow")
                }
                .padding(.horizontal, Theme.Spacing.s)
            }
            Theme.Colors.borderSoft.frame(height: WaddleTokens.BorderWidth.hair)
            HStack {
                Spacer()
                Button("Cancel", action: cancel)
                    .buttonStyle(.huddle(size: .small))
                    .keyboardShortcut(.cancelAction)
            }
            .padding(Theme.Spacing.m)
        }
        .frame(width: 420, height: 460)
        .background(Theme.Colors.bg)
    }

    @ViewBuilder
    private func section(_ title: String, _ rows: [CallScreenSource], symbol: String) -> some View {
        if !rows.isEmpty {
            Text(title)
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                .foregroundStyle(Theme.Colors.fgMuted)
                .padding(.horizontal, Theme.Spacing.s)
                .padding(.top, Theme.Spacing.s)
                .accessibilityAddTraits(.isHeader)
            ForEach(rows) { row in
                CallScreenPickerRow(source: row, symbol: symbol) { pick(row) }
            }
        }
    }
}

private struct CallScreenPickerRow: View {
    let source: CallScreenSource
    let symbol: String
    let action: @MainActor () -> Void
    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: WaddleTokens.Spacing.s2Half) {
                Image(systemName: symbol)
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: 0) {
                    Text(source.title)
                        .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                        .foregroundStyle(Theme.Colors.fg)
                    if !source.detail.isEmpty {
                        Text(source.detail)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgMuted)
                    }
                }
                .lineLimit(1)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.Spacing.s)
            .padding(.vertical, Theme.Spacing.s)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                    .fill(isHovered ? Theme.State.hover : .clear)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .accessibilityLabel(Text(source.detail.isEmpty ? "Share \(source.title)" : "Share \(source.title), \(source.detail)"))
    }
}
