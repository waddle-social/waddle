import SwiftUI

// MARK: - Member Panes

private struct ChatDesktopMemberRowView: View {
    let member: ChatRoomMember

    var body: some View {
        HStack(spacing: 10) {
            Text(member.avatarInitials ?? initials(from: member.displayName))
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .frame(width: 30, height: 30)
                .background(presenceColor.opacity(0.16), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(alignment: .bottomTrailing) {
                    Circle()
                        .fill(presenceColor)
                        .frame(width: 9, height: 9)
                }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(member.displayName)
                        .font(.subheadline.weight(.medium))
                        .lineLimit(1)
                    if member.isSelf {
                        Text("you")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(.quaternary.opacity(0.6), in: Capsule())
                    }
                }

                HStack(spacing: 6) {
                    Text(member.presence.label)
                    if let role = member.role, !role.isEmpty {
                        Text("•")
                        Text(role)
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(WaddleTheme.surfaceRaised)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
        }
    }

    private var presenceColor: Color {
        switch member.presence {
        case .available: return WaddleTheme.presenceOnline
        case .away:      return WaddleTheme.presenceAway
        case .dnd:       return WaddleTheme.presenceDnd
        case .offline, .unknown: return WaddleTheme.presenceOffline
        }
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }
}

extension WaddleChatSpaceView {
    var desktopMemberPane: some View {
        let activeMembers = model.chatMembers.filter(isInteractivePresence)
        let quietMembers = model.chatMembers.filter { !isInteractivePresence($0) }

        return VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("Members")
                        .font(.system(size: 17, weight: .semibold))
                    Button {
                        withAnimation(.easeOut(duration: 0.18)) {
                            showDesktopMemberPane = false
                        }
                    } label: {
                        Image(systemName: "sidebar.trailing")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    Spacer(minLength: 12)
                    Text("\(model.chatMembers.count)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(WaddleTheme.surfaceRaised, in: Capsule())
                        .overlay {
                            Capsule()
                                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                        }
                }

                HStack(spacing: 8) {
                    desktopSidebarStat(title: "Active", value: activeMembers.count)
                    desktopSidebarStat(title: "Quiet", value: quietMembers.count)
                }
            }
            .padding(18)

            Divider()
                .overlay(WaddleTheme.divider)

            if model.chatMembers.isEmpty {
                ChatEmptyStateView(
                    title: "No members loaded",
                    message: "Channel participants will appear here.",
                    systemImage: "person.2"
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        if !activeMembers.isEmpty {
                            desktopMemberSection(title: "Active now", members: activeMembers)
                        }

                        if !quietMembers.isEmpty {
                            desktopMemberSection(title: activeMembers.isEmpty ? "Members" : "Quiet", members: quietMembers)
                        }
                    }
                    .padding(18)
                }
            }
        }
        .background(WaddleTheme.sidebarBackground)
    }

    var memberPane: some View {
        ScrollView {
            ChatMemberListSection(members: model.chatMembers)
                .padding(16)
        }
        .background(WaddleTheme.sidebarBackground)
    }

    private func desktopMemberSection(title: String, members: [ChatRoomMember]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            VStack(spacing: 6) {
                ForEach(members) { member in
                    ChatDesktopMemberRowView(member: member)
                        .contextMenu {
                            if !member.isSelf {
                                Button {
                                    let peerJID = "\(member.displayName.lowercased())@\(jidDomain(model.session?.jid ?? ""))"
                                    Task { await model.openDm(peerJID: peerJID, peerUsername: member.displayName) }
                                } label: {
                                    Label("Message", systemImage: "bubble.left")
                                }

                                Menu("Change Role") {
                                    ForEach(["member", "moderator", "admin"], id: \.self) { role in
                                        Button(role.capitalized) {
                                            Task { await model.changeMemberRole(userID: member.id, role: role) }
                                        }
                                    }
                                }

                                Divider()

                                Button(role: .destructive) {
                                    Task { await model.removeMember(userID: member.id) }
                                } label: {
                                    Label("Remove", systemImage: "person.badge.minus")
                                }
                            }
                        }
                }
            }
        }
    }

    private func desktopSidebarStat(title: String, value: Int) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("\(value)")
                .font(.subheadline.weight(.semibold))
            Text(title)
                .font(.caption)
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
        }
    }
}
