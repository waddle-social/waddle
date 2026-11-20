import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import WaddleKit

/// Posts a story: pick a picture, say something about it if you like.
struct StoryComposerSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var composer: StoryComposer
    @State private var pickedItem: PhotosPickerItem?

    /// Snapshots hand in a composer already in the state they show.
    init(composer: StoryComposer? = nil) {
        _composer = State(initialValue: composer ?? StoryComposer())
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                    picture
                    if composer.picture != nil {
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
                    Button("Cancel") { dismiss() }
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
            Task {
                guard let data = try? await item.loadTransferable(type: Data.self) else {
                    composer.errorMessage = "Couldn't read that picture. Try another one."
                    return
                }
                let type = item.supportedContentTypes.first
                await composer.pick(data, mediaType: type?.preferredMIMEType, fileExtension: type?.preferredFilenameExtension)
            }
        }
    }

    @ViewBuilder
    private var picture: some View {
        if let picked = composer.picture, let image = Image(storyPicture: picked.data) {
            image
                .resizable()
                .scaledToFit()
                .frame(maxWidth: .infinity, maxHeight: 360)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
                .overlay(alignment: .bottom) { progress }
                .accessibilityLabel(Text("The picture for your story"))
            PhotosPicker(selection: $pickedItem, matching: .images) {
                Label("Choose another picture", systemImage: "photo.on.rectangle")
            }
            .buttonStyle(.huddle(size: .small))
            .disabled(composer.isPosting)
        } else {
            PhotosPicker(selection: $pickedItem, matching: .images) {
                VStack(spacing: Theme.Spacing.s) {
                    Image(systemName: "photo.badge.plus")
                        .font(.system(size: 34))
                        .foregroundStyle(Theme.Colors.actionText)
                        .accessibilityHidden(true)
                    Text("Choose a picture")
                        .huddleText(Theme.Typography.button)
                        .foregroundStyle(Theme.Colors.fg)
                }
                .frame(maxWidth: .infinity, minHeight: 220)
                .huddleSurface(.card, radius: Theme.Radius.card)
            }
            .buttonStyle(.plain)
        }
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
        Task {
            if await composer.post(using: session) {
                dismiss()
            }
        }
    }
}

private extension Image {
    init?(storyPicture data: Data) {
        #if os(macOS)
        guard let image = NSImage(data: data) else { return nil }
        self.init(nsImage: image)
        #else
        guard let image = UIImage(data: data) else { return nil }
        self.init(uiImage: image)
        #endif
    }
}
