import SwiftUI
import WaddleKit

/// Who is in a room (Android `members` frame): how many, a Here now /
/// Everyone switch, a card per person, then the room's bots. A card opens
/// the person's profile; its context menu holds what we may do to them.
struct RoomMembersView: View {
    enum Filter: Hashable {
        case here
        case everyone
    }

    @Environment(SessionCoordinator.self) private var session
    let room: BareJID
    let model: RoomMembersModel
    @State private var filter: Filter

    init(room: BareJID, model: RoomMembersModel, filter: Filter = .here) {
        self.room = room
        self.model = model
        _filter = State(initialValue: filter)
    }

    var body: some View {
        let present = Array((session.presence.occupants[room] ?? [:]).values)
        let cards = MemberRoster.cards(present: present, members: model.members, contacts: session.presence.contacts)
        let here = cards.filter(\.isHere)
        let shown = filter == .here ? here : cards
        let bots = filter == .everyone ? session.roomBots.bots(in: room) : []
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                RoomMembersHeader(
                    title: session.directory.title(for: .room(room)),
                    total: cards.count,
                    here: here.count,
                    filter: $filter
                )
                .padding(.bottom, Theme.Spacing.s)
                if filter == .everyone {
                    RoomMembersLoadNotice(room: room, model: model)
                }
                if shown.isEmpty && !(filter == .everyone && model.members == nil) {
                    RoomMembersEmpty(isFiltered: filter == .here && !cards.isEmpty)
                }
                ForEach(shown) { card in
                    MemberCardRow(room: room, card: card, model: model)
                }
                if !bots.isEmpty {
                    Text("Bots")
                        .huddleText(Theme.Typography.eyebrow)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .padding(.top, Theme.Spacing.l)
                        .padding(.bottom, Theme.Spacing.xxs)
                        .accessibilityAddTraits(.isHeader)
                    ForEach(bots) { bot in
                        BotCardRow(bot: bot)
                    }
                }
            }
            .padding(Theme.Spacing.l)
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Members")
        .task {
            // Everyone shows the affiliation lists; load them now so the
            // count is whole by the time anyone switches.
            if model.members == nil {
                await model.loadMembers(using: session)
            }
        }
        .memberModerationDialogs(model)
    }
}

/// "In #design", the count, how many are here, and the switch.
private struct RoomMembersHeader: View {
    let title: String
    let total: Int
    let here: Int
    @Binding var filter: RoomMembersView.Filter

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text("In \(title)")
                .huddleText(Theme.Typography.eyebrow)
                .foregroundStyle(Theme.Colors.fgMuted)
            Text(total == 1 ? "1 member" : "\(total) members")
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
            Text("\(here) here now. Pick someone to see their profile.")
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
            HuddleSegmentedControl(selection: $filter, segments: [
                .init(value: .here, title: "Here now"),
                .init(value: .everyone, title: "Everyone"),
            ])
            .padding(.top, Theme.Spacing.s)
        }
    }
}

/// While Everyone loads, or why it didn't.
private struct RoomMembersLoadNotice: View {
    @Environment(SessionCoordinator.self) private var session
    let room: BareJID
    let model: RoomMembersModel

    var body: some View {
        if model.isLoading && model.members == nil {
            HStack(spacing: Theme.Spacing.s) {
                ProgressView()
                    .controlSize(.small)
                Text("Loading everyone")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .padding(.vertical, Theme.Spacing.s)
        } else if let error = model.loadError {
            HStack(spacing: Theme.Spacing.s) {
                Text(model.members == nil ? error : "Showing the last members we loaded.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.warningText)
                Spacer(minLength: 0)
                Button("Try again") {
                    let session = self.session
                    Task { await model.loadMembers(using: session) }
                }
                .buttonStyle(.huddle(size: .small))
            }
            .padding(.vertical, Theme.Spacing.xs)
        }
    }
}

private struct RoomMembersEmpty: View {
    /// Here now is empty but Everyone is not.
    let isFiltered: Bool

    var body: some View {
        VStack(spacing: Theme.Spacing.xs) {
            Text("It's quiet in here")
                .huddleText(Theme.Typography.subtitle)
                .foregroundStyle(Theme.Colors.fg)
            Text(isFiltered ? "Nobody is here right now. Switch to Everyone to see the whole list." : "Be the one who breaks the silence.")
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.xl)
        .accessibilityElement(children: .combine)
    }
}

extension View {
    /// The ban confirmation and the failure alert for member actions.
    func memberModerationDialogs(_ model: RoomMembersModel) -> some View {
        modifier(MemberModerationDialogs(model: model))
    }
}

private struct MemberModerationDialogs: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    let model: RoomMembersModel

    func body(content: Content) -> some View {
        content
            .confirmationDialog(
                "Ban this person?",
                isPresented: banBinding,
                titleVisibility: .visible,
                presenting: model.pendingBan
            ) { _ in
                Button("Ban", role: .destructive) {
                    model.confirmBan(using: session)
                }
                Button("Cancel", role: .cancel) {
                    model.pendingBan = nil
                }
            } message: { subject in
                Text("\(subject.nick ?? subject.jid?.description ?? "They") will be removed and can't rejoin until the ban is lifted.")
            }
            .alert("Couldn't update member", isPresented: errorBinding) {
                Button("OK", role: .cancel) {
                    model.actionError = nil
                }
            } message: {
                Text(model.actionError ?? "")
            }
    }

    private var banBinding: Binding<Bool> {
        Binding(
            get: { model.pendingBan != nil },
            set: { if !$0 { model.pendingBan = nil } }
        )
    }

    private var errorBinding: Binding<Bool> {
        Binding(
            get: { model.actionError != nil },
            set: { if !$0 { model.actionError = nil } }
        )
    }
}
