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
            .dropDestination(for: URL.self) { urls, _ in
                let files = urls.filter(\.isFileURL)
                intake.attachFiles(files)
                return !files.isEmpty
            }
            .onChange(of: photoSelection) { _, items in
                guard !items.isEmpty else { return }
                photoSelection = []
                intake.attachPhotos(items)
            }
    }
}
