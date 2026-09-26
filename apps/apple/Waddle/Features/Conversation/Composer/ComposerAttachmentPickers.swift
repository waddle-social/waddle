import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The photo picker, file importer and drop target that feed the
/// composer's attachments.
struct ComposerAttachmentPickers: ViewModifier {
    @Binding var showsPhotoPicker: Bool
    @Binding var showsFileImporter: Bool
    @Binding var photoSelection: [PhotosPickerItem]
    let intake: ComposerAttachmentIntake
    @State private var isDropTargeted = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .photosPicker(
                isPresented: $showsPhotoPicker,
                selection: $photoSelection,
                maxSelectionCount: 6,
                // Images only: picked assets load into memory, and video
                // location metadata is not stripped. Videos go through Files.
                matching: .images
            )
            .fileImporter(isPresented: $showsFileImporter, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                if case let .success(urls) = result {
                    intake.attachFiles(urls)
                }
            }
            .onDrop(of: [.fileURL, .image], isTargeted: $isDropTargeted) { providers in
                intake.attachDropped(providers)
            }
            .overlay {
                if isDropTargeted {
                    ComposerDropHighlight()
                        .transition(.opacity)
                }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.15), value: isDropTargeted)
            .onChange(of: photoSelection) { _, items in
                guard !items.isEmpty else { return }
                photoSelection = []
                intake.attachPhotos(items)
            }
    }
}

/// Shown over the composer while a file or picture is dragged over it.
private struct ComposerDropHighlight: View {
    var body: some View {
        RoundedRectangle(cornerRadius: ComposerMetrics.cardRadius, style: .continuous)
            .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
            .background(
                RoundedRectangle(cornerRadius: ComposerMetrics.cardRadius, style: .continuous)
                    .fill(Color.accentColor.opacity(0.1))
            )
            .overlay {
                Label("Drop to attach", systemImage: "paperclip")
                    .font(.callout.weight(.semibold))
                    .foregroundStyle(Color.accentColor)
            }
            .padding(Theme.Spacing.xs)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}
