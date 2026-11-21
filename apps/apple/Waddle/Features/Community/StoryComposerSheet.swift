import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import WaddleKit

/// Posts a story: pick a picture or a video, say something about it if you
/// like.
struct StoryComposerSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var composer: StoryComposer
    @State private var pickedItem: PhotosPickerItem?
    @State private var posting: Task<Void, Never>?
    @ScaledMetric(relativeTo: .largeTitle) private var addGlyph: CGFloat = 34
    @ScaledMetric(relativeTo: .title) private var playGlyph: CGFloat = 22

    /// Snapshots hand in a composer already in the state they show.
    init(composer: StoryComposer? = nil) {
        _composer = State(initialValue: composer ?? StoryComposer())
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                    media
                    if composer.media != nil {
                        caption
                    }
                    if let errorMessage = composer.errorMessage {
                        Label(errorMessage, systemImage: "exclamationmark.triangle")
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.dangerText)
                    }
                    Text("Everyone in your community can see a story for a day.")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
                .padding(Theme.Spacing.l)
            }
            .background(Theme.Colors.bg)
            .navigationTitle("New story")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        posting?.cancel()
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(composer.isPosting ? "Posting…" : "Post", action: post)
                        .disabled(!composer.canPost)
                }
            }
            .interactiveDismissDisabled(composer.isPosting)
        }
        #if os(macOS)
        .frame(width: 440, height: 600)
        #endif
        .onChange(of: pickedItem) { _, item in
            guard let item else { return }
            let type = item.supportedContentTypes.first
            Task {
                if item.supportedContentTypes.contains(where: { $0.conforms(to: .movie) }) {
                    await composer.pickVideo { try? await item.loadTransferable(type: PickedMovie.self)?.url }
                } else {
                    await composer.pick(mediaType: type?.preferredMIMEType, fileExtension: type?.preferredFilenameExtension) {
                        try? await item.loadTransferable(type: Data.self)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var media: some View {
        if let media = composer.media {
            chosenPreview
                .overlay { if media.isVideo { videoMark } }
                .overlay(alignment: .bottom) { progress }
                .accessibilityElement()
                .accessibilityLabel(Text(media.isVideo ? "The video for your story" : "The picture for your story"))
            picker {
                Label(
                    media.isVideo ? "Choose another video" : "Choose another picture",
                    systemImage: "photo.on.rectangle"
                )
            }
            .buttonStyle(.huddle(size: .small))
            .disabled(composer.isPosting)
        } else {
            picker {
                VStack(spacing: Theme.Spacing.s) {
                    if composer.isPicking {
                        ProgressView()
                            .accessibilityLabel(Text("Loading what you chose"))
                    } else {
                        Image(systemName: "photo.badge.plus")
                            .font(.system(size: addGlyph))
                            .foregroundStyle(Theme.Colors.actionText)
                            .accessibilityHidden(true)
                    }
                    Text("Choose a picture or video")
                        .huddleText(Theme.Typography.button)
                        .foregroundStyle(Theme.Colors.fg)
                    Text("Up to \(composer.limitText)")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
                .frame(maxWidth: .infinity, minHeight: 220)
                .huddleSurface(.card, radius: Theme.Radius.card)
            }
            .buttonStyle(.plain)
        }
    }

    /// The picture, or the video's first frame; a plain ground for a video
    /// whose first frame could not be made.
    @ViewBuilder
    private var chosenPreview: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
        if let preview = composer.preview {
            preview
                .resizable()
                .scaledToFit()
                .frame(maxWidth: .infinity, maxHeight: 360)
                .clipShape(shape)
        } else {
            shape
                .fill(Theme.Colors.surface2)
                .frame(maxWidth: .infinity, minHeight: 220, maxHeight: 220)
        }
    }

    /// Pictures and videos; a video comes in the most compatible encoding
    /// the library has (H.264 rather than HEVC), so every client plays it.
    private func picker(@ViewBuilder label: () -> some View) -> some View {
        PhotosPicker(
            selection: $pickedItem,
            matching: .any(of: [.images, .videos]),
            preferredItemEncoding: .compatible,
            label: label
        )
    }

    /// Says the preview is a video's first frame.
    private var videoMark: some View {
        Image(systemName: "play.fill")
            .font(.system(size: playGlyph, weight: .semibold))
            .foregroundStyle(Theme.Colors.fg)
            .padding(Theme.Spacing.l)
            .background(Theme.Colors.scrim, in: Circle())
            .environment(\.colorScheme, .dark)
            .accessibilityHidden(true)
    }

    @ViewBuilder
    private var progress: some View {
        if case let .posting(fraction) = composer.phase {
            ProgressView(value: fraction)
                .tint(Theme.Colors.action)
                .padding(Theme.Spacing.m)
                .accessibilityLabel(Text("Uploading"))
        }
    }

    private var caption: some View {
        TextField("Add a caption", text: $composer.caption, axis: .vertical)
            .textFieldStyle(.plain)
            .huddleText(Theme.Typography.input)
            .foregroundStyle(Theme.Colors.fg)
            .lineLimit(1...4)
            .padding(Theme.Spacing.m)
            .huddleSurface(.card, radius: Theme.Radius.medium)
            .environment(\.huddleGround, Theme.Colors.surface)
            .disabled(composer.isPosting)
            .accessibilityLabel(Text("Caption"))
    }

    private func post() {
        posting = Task {
            if await composer.post(using: session) {
                dismiss()
            }
        }
    }
}
