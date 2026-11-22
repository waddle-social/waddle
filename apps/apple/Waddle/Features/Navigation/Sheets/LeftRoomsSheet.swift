import Observation
import SwiftUI
import WaddleKit

/// Rejoining from the list of rooms the account left: one join at a time,
/// and what went wrong when one fails.
@MainActor
@Observable
final class LeftRoomsModel {
    /// The room being joined, if any.
    private(set) var joining: BareJID?
    var failure: String?

    /// Rejoins `room` and opens it. Returns whether it was joined.
    @discardableResult
    func join(_ room: BareJID, using session: SessionCoordinator, navigation: NavigationModel) async -> Bool {
        guard joining == nil else { return false }
        joining = room
        failure = nil
        defer { joining = nil }
        do {
            try await session.rejoinRoom(room)
            navigation.open(.room(room))
            return true
        } catch RoomRejoinError.roomIsGone {
            failure = "That room no longer exists."
        } catch {
            failure = ActionErrorCopy.message(for: error, fallback: "Couldn't join the room. Try again.")
        }
        return false
    }
}

/// The rooms the account left, each with a way back in.
struct LeftRoomsSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @State private var model: LeftRoomsModel

    init(model: LeftRoomsModel? = nil) {
        _model = State(initialValue: model ?? LeftRoomsModel())
    }

    var body: some View {
        NavigationStack {
            Form {
                if let failure = model.failure {
                    Section {
                        Label(failure, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(Theme.Colors.dangerText)
                    }
                }
                Section {
                    ForEach(session.directory.leftRooms) { channel in
                        LeftRoomRow(channel: channel, isJoining: model.joining == channel.roomJID) {
                            join(channel.roomJID)
                        }
                    }
                } footer: {
                    Text("You get no messages or notifications from these rooms on any of your devices.")
                }
            }
            .formStyle(.grouped)
            .disabled(model.joining != nil)
            .navigationTitle("Rooms you left")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .leftRoomsSheetFrame()
    }

    private func join(_ room: BareJID) {
        let session = self.session
        let navigation = self.navigation
        Task {
            if await model.join(room, using: session, navigation: navigation) { dismiss() }
        }
    }
}

private struct LeftRoomRow: View {
    let channel: Channel
    let isJoining: Bool
    let onJoin: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            RoomSymbolTile(symbol: ChannelSymbol.name(for: channel))
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(channel.name)
                    .foregroundStyle(Theme.Colors.fg)
                if let summary = channel.summary, !summary.isEmpty {
                    Text(summary)
                        .font(.footnote)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: Theme.Spacing.s)
            if isJoining {
                ProgressView()
                    .controlSize(.small)
            } else {
                Button("Join", action: onJoin)
                    .buttonStyle(.huddle(size: .small))
                    .accessibilityLabel(Text("Join \(channel.name)"))
            }
        }
    }
}

private extension View {
    func leftRoomsSheetFrame() -> some View {
        #if os(macOS)
        return self.frame(minWidth: 420, idealWidth: 460, minHeight: 320)
        #else
        return self
        #endif
    }
}
