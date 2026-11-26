import SwiftUI

/// The status under a call's name: what the call is doing, or its clock
/// with the live dot while it is in progress.
struct CallStatusLine: View {
    let text: String
    /// In progress: the live dot shows and the text is the ticking clock.
    var isLive = false
    var isEmphasised = false
    var color = Theme.Colors.fgMuted

    var body: some View {
        HStack(spacing: Theme.Spacing.xs) {
            if isLive {
                Circle()
                    .fill(Theme.Colors.live)
                    .frame(width: 8, height: 8)
                    .accessibilityHidden(true)
            }
            Text(text)
                .huddleText(Theme.Typography.label.weight(isEmphasised ? 600 : 400))
                .monospacedDigit()
                .foregroundStyle(color)
                .accessibilityAddTraits(isLive ? .updatesFrequently : [])
        }
    }
}

/// A call control with its name beside its glyph.
struct CallLabelledButton: View {
    let title: String
    let symbol: String
    let variant: HuddleButtonStyle.Variant
    var stretched = false
    let action: @MainActor () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: symbol)
                .frame(maxWidth: stretched ? .infinity : nil)
        }
        .buttonStyle(.huddle(variant))
    }
}

/// An icon-only call control. Its glyph box scales with the text, so the
/// control stays square as text grows; stretched, it shares the row.
struct CallIconToggle: View {
    let title: String
    let symbol: String
    var tint = Theme.Colors.fg
    var stretched = false
    let action: @MainActor () -> Void

    @ScaledMetric(relativeTo: .body) private var iconWidth: CGFloat = 20

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .foregroundStyle(tint)
                .frame(minWidth: iconWidth, maxWidth: stretched ? .infinity : iconWidth)
        }
        .buttonStyle(.huddle())
        .accessibilityLabel(Text(title))
        .help(title)
    }
}

/// The dock's way to a call's stage on a phone, where the stage covers
/// the screen once it is opened; nothing where there is no such stage.
struct CallOpenButton: View {
    var stretched = false
    let action: (@MainActor () -> Void)?

    var body: some View {
        if let action {
            CallIconToggle(
                title: "Open call", symbol: "arrow.up.left.and.arrow.down.right", stretched: stretched, action: action
            )
        }
    }
}

struct CallDockVideo {
    var remote: AnyView?
    var preview: AnyView?
    /// Whose screen `remote` is, when it is a shared screen and not a camera.
    var screenSharer: String?

    var hasCamera: Bool { remote != nil || preview != nil }
}

/// The remote camera in a 16:9 frame, or `placeholder` while there is
/// none, with our own preview inset in the corner.
struct CallVideoStage<Placeholder: View>: View {
    let video: CallDockVideo
    /// What VoiceOver says for the stage.
    let label: String
    @ViewBuilder let placeholder: Placeholder

    @Environment(\.horizontalSizeClass) private var sizeClass

    var body: some View {
        // Concentric with the card: its radius less its padding, floored.
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
        ZStack {
            Theme.Colors.surface2
            if let remote = video.remote {
                remote
            } else {
                placeholder
            }
        }
        .overlay {
            if let preview = video.preview {
                GeometryReader { stage in
                    localPreview(preview, width: stage.size.width * CallVideoMetrics.previewShare)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                }
            }
        }
        .aspectRatio(16 / 9, contentMode: .fit)
        .clipShape(shape)
        .frame(
            maxWidth: .infinity,
            maxHeight: sizeClass == .compact ? CallVideoMetrics.compactStageHeight : CallVideoMetrics.stageHeight
        )
        .accessibilityElement()
        .accessibilityLabel(Text(label))
    }

    private func localPreview(_ video: AnyView, width: CGFloat) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
        return video
            .frame(width: width, height: width / CallVideoMetrics.previewAspect)
            .clipShape(shape)
            .overlay(shape.strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair))
            .padding(Theme.Spacing.s)
    }
}

enum CallVideoMetrics {
    static let stageHeight: CGFloat = 360
    static let compactStageHeight: CGFloat = 240
    /// Our preview's share of the stage's width.
    static let previewShare: CGFloat = 0.25
    /// Width over height: held upright on a phone, landscape on a Mac.
    #if os(iOS)
    static let previewAspect: CGFloat = 3 / 4
    #else
    static let previewAspect: CGFloat = 16 / 9
    #endif
}
