import SwiftUI
import UniformTypeIdentifiers
import WaddleKit

/// A single destination covering the timeline and its composer.
struct ConversationDropTarget: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isTargeted = false

    let composer: ComposerModel

    /// Files, and pictures that arrive as image data. Accepted while a message is being edited too,
    /// so the intake can say why nothing was attached.
    private static let attachmentTypes: [UTType] = [.fileURL, .image]

    func body(content: Content) -> some View {
        let showsOverlay = isTargeted
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentShape(Rectangle())
            .onDrop(of: Self.attachmentTypes, isTargeted: $isTargeted) { providers in
                isTargeted = false
                return ComposerAttachmentIntake(
                    model: composer,
                    uploader: AttachmentUploader(session: session, protection: .encrypted)
                ).attachDropped(providers)
            }
            .overlay {
                ZStack {
                    if showsOverlay {
                        ConversationDropOverlay()
                            .transition(.opacity)
                    }
                }
                .animation(
                    reduceMotion ? nil : .easeOut(duration: WaddleTokens.Duration.fast),
                    value: showsOverlay
                )
            }
            .onChange(of: showsOverlay) { _, visible in
                if visible {
                    let title = String(localized: ConversationDropOverlay.title)
                    AccessibilityNotification.Announcement(title).post()
                }
            }
    }
}

struct ConversationDropOverlay: View {
    static let title: LocalizedStringResource = "Drop files to upload"

    var body: some View {
        let panel = RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
        let border = StrokeStyle(lineWidth: WaddleTokens.BorderWidth.strong, dash: [6, 4])
        panel
            .fill(Theme.Colors.actionSubtle)
            .overlay {
                panel.strokeBorder(Theme.Colors.actionText, style: border)
            }
            .overlay {
                VStack(spacing: Theme.Spacing.s) {
                    Image(systemName: "tray.and.arrow.up")
                        .font(.system(size: Theme.Size.sharingGlyph))
                    Text(Self.title)
                        .huddleText(Theme.Typography.subtitle)
                        .multilineTextAlignment(.center)
                }
                .foregroundStyle(Theme.Colors.actionText)
                .padding(Theme.Spacing.m)
            }
            .padding(Theme.Spacing.m)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}
