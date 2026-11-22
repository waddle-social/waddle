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
            // Android `profile`: a pill under the header.
            HStack(spacing: Theme.Spacing.s) {
                PhotosPicker(selection: $pickedItem, matching: .images) {
                    Label(hasAvatar ? "Change avatar" : "Add avatar", systemImage: "camera")
                }
                .buttonStyle(.huddle(size: .small))
                if hasAvatar {
                    Button("Remove avatar", role: .destructive, action: remove)
                        .buttonStyle(.huddle(.danger, size: .small))
                }
                if isWorking {
                    ProgressView()
                        .controlSize(.small)
                        .accessibilityLabel(Text("Updating avatar"))
                }
            }
            .frame(maxWidth: .infinity)
            .disabled(isWorking)
            if let errorMessage {
                Text(errorMessage)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
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
