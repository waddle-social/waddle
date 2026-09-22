import PhotosUI
import SwiftUI
import WaddleKit

/// Change or remove the XEP-0084 avatar.
struct ProfileAvatarControls: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var pickedItem: PhotosPickerItem?
    @State private var isWorking = false
    @State private var errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            HStack(spacing: Theme.Spacing.m) {
                PhotosPicker(selection: $pickedItem, matching: .images) {
                    Label(hasAvatar ? "Change photo" : "Add photo", systemImage: "photo.on.rectangle")
                }
                if hasAvatar {
                    Button("Remove photo", role: .destructive, action: remove)
                }
                Spacer(minLength: 0)
                if isWorking {
                    ProgressView()
                        .controlSize(.small)
                        .accessibilityLabel(Text("Updating photo"))
                }
            }
            .buttonStyle(.borderless)
            .disabled(isWorking)
            if let errorMessage {
                Text(errorMessage)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
        }
        .onChange(of: pickedItem) { _, item in
            guard let item else { return }
            publish(item)
        }
    }

    private var hasAvatar: Bool {
        session.avatars.image(for: session.account.jid) != nil
    }

    private func publish(_ item: PhotosPickerItem) {
        run {
            guard let data = try await item.loadTransferable(type: Data.self) else {
                throw ProfileAvatarEncoder.Failure.unreadable
            }
            let avatar = try await Task.detached(priority: .userInitiated) {
                try ProfileAvatarEncoder.encode(data)
            }.value
            try await session.publishAvatar(avatar)
        }
    }

    private func remove() {
        run { try await session.removeAvatar() }
    }

    private func run(_ work: @escaping () async throws -> Void) {
        isWorking = true
        errorMessage = nil
        Task {
            do {
                try await work()
            } catch {
                errorMessage = error.localizedDescription
            }
            isWorking = false
            pickedItem = nil
        }
    }
}
