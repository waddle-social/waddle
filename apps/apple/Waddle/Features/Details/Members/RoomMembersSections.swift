import SwiftUI
import WaddleKit

/// Present occupants by XEP-0045 role, then affiliated members who are
/// away once the affiliation lists are loaded.
struct RoomMembersSections: View {
    @Environment(SessionCoordinator.self) private var session
    let room: BareJID
    let model: RoomMembersModel

    var body: some View {
        let occupants = Array((session.presence.occupants[room] ?? [:]).values)
        ForEach(MemberRoster.grouped(occupants)) { group in
            Section {
                ForEach(group.occupants) { occupant in
                    OccupantMemberRow(room: room, occupant: occupant, model: model)
                }
            } header: {
                Text("\(MemberLabels.groupTitle(group.role)) · \(group.occupants.count)")
            }
        }
        AbsentMembersSection(room: room, model: model, occupants: occupants)
    }
}

/// "Load all members" and, once loaded, the members not in the room.
struct AbsentMembersSection: View {
    @Environment(SessionCoordinator.self) private var session
    let room: BareJID
    let model: RoomMembersModel
    let occupants: [Occupant]

    var body: some View {
        Section {
            if let absent = model.absentMembers(present: occupants) {
                ForEach(absent) { member in
                    AbsentMemberRow(room: room, member: member, model: model)
                }
                if absent.isEmpty {
                    Text("Every member is here.")
                        .foregroundStyle(.secondary)
                }
            } else {
                loadButton
            }
            if let error = model.loadError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.subheadline)
                    .foregroundStyle(.red)
            }
        } header: {
            Text(model.members == nil ? "All members" : "Not here")
        }
    }

    private var loadButton: some View {
        Button {
            let session = self.session
            Task { await model.loadMembers(using: session) }
        } label: {
            HStack(spacing: Theme.Spacing.s) {
                Label("Load all members", systemImage: "person.3")
                Spacer(minLength: 0)
                if model.isLoading {
                    ProgressView()
                        .controlSize(.small)
                }
            }
        }
        .disabled(model.isLoading)
    }
}
