import SwiftUI

/// The filled `+` menu: attach a photo, file or GIF, paste, or browse
/// slash commands.
struct ComposerPlusMenu: View {
    let isEditing: Bool
    let actions: ComposerActions

    var body: some View {
        Menu {
            Button(action: actions.pickPhoto) {
                Label("Photo", systemImage: "photo.on.rectangle")
            }
            Button(action: actions.pickFile) {
                Label("File", systemImage: "doc")
            }
            Button(action: actions.pickGIF) {
                Label("GIF", systemImage: "play.rectangle.on.rectangle")
            }
            Button(action: actions.paste) {
                Label("Paste", systemImage: "doc.on.clipboard")
            }
            Divider()
            Button(action: actions.startCommand) {
                Label("Commands…", systemImage: "slash.circle")
            }
        } label: {
            Image(systemName: "plus")
                .font(.system(size: ComposerMetrics.iconSize - 2, weight: .bold))
                .foregroundStyle(Color.primary)
                .frame(width: ComposerMetrics.plusDiameter, height: ComposerMetrics.plusDiameter)
                .background(Circle().fill(Color.secondary.opacity(0.18)))
                .frame(width: ComposerMetrics.touchTarget, height: ComposerMetrics.touchTarget)
                .contentShape(Rectangle())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(isEditing)
        .help("Add")
        .accessibilityLabel(Text("Add"))
    }
}
