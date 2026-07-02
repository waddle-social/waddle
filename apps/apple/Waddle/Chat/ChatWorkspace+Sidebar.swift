import SwiftUI

// MARK: - Compact Sidebar

extension WaddleChatSpaceView {
    var compactSidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Text(space.name.prefix(2).uppercased())
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.white)
                    .frame(width: 32, height: 32)
                    .background(WaddleTheme.accent, in: RoundedRectangle(cornerRadius: 8))

                VStack(alignment: .leading, spacing: 1) {
                    Text(space.name)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(WaddleTheme.textPrimary)
                    Text("\(model.members.count) members")
                        .font(.caption2)
                        .foregroundStyle(WaddleTheme.textMuted)
                }
                Spacer()
            }
            .padding(16)

            WaddleTheme.divider.frame(height: 1)

            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    sidebarSectionHeader("Channels", action: { showCreateChannelSheet = true })

                    ForEach(channelGroups) { group in
                        if model.spaces.count > 1 {
                            sidebarSpaceHeader(group.space.name)
                        }
                        ForEach(group.rooms) { room in
                            sidebarChannelRow(room: room)
                        }
                    }

                    if !store.dmConversations.isEmpty {
                        sidebarSectionHeader("Direct Messages", action: { showNewDmSheet = true })

                        ForEach(store.dmConversations) { convo in
                            sidebarDmRow(convo: convo)
                        }
                    }
                }
                .padding(.vertical, 8)
            }
        }
    }

    private func sidebarSectionHeader(_ title: String, action: @escaping () -> Void) -> some View {
        HStack {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textMuted)
                .textCase(.uppercase)
            Spacer()
            Button(action: action) {
                Image(systemName: "plus")
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
    }

    private func sidebarSpaceHeader(_ title: String) -> some View {
        Text(title)
            .font(.caption2.weight(.semibold))
            .foregroundStyle(WaddleTheme.textMuted)
            .lineLimit(1)
            .padding(.horizontal, 16)
            .padding(.top, 10)
            .padding(.bottom, 3)
    }

    private func sidebarChannelRow(room: ChatRoomSelection) -> some View {
        Button {
            Task { await model.selectChannel(room.id) }
            showChannelSidebar = false
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "number")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(model.selectedChannelID == room.id ? WaddleTheme.textPrimary : WaddleTheme.textMuted)
                    .frame(width: 18)
                Text(room.title)
                    .font(WaddleTheme.channelFont)
                    .foregroundStyle(model.selectedChannelID == room.id ? WaddleTheme.textPrimary : WaddleTheme.textSecondary)
                    .lineLimit(1)
                Spacer()
                if room.unreadCount > 0 {
                    Text("\(room.unreadCount)")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(WaddleTheme.unreadBadge, in: Capsule())
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(model.selectedChannelID == room.id ? WaddleTheme.channelSelected : Color.clear, in: RoundedRectangle(cornerRadius: 6))
            .padding(.horizontal, 8)
        }
        .buttonStyle(.plain)
    }

    private func sidebarDmRow(convo: DmConversation) -> some View {
        Button {
            Task { await model.openDm(peerJID: convo.peerJID, peerUsername: convo.peerUsername) }
            showChannelSidebar = false
        } label: {
            HStack(spacing: 8) {
                Circle()
                    .fill(convo.presenceShow == .available ? WaddleTheme.presenceOnline : WaddleTheme.presenceOffline)
                    .frame(width: 8, height: 8)
                Text(convo.peerUsername)
                    .font(WaddleTheme.channelFont)
                    .foregroundStyle(WaddleTheme.textSecondary)
                    .lineLimit(1)
                Spacer()
                if convo.unreadCount > 0 {
                    Text("\(convo.unreadCount)")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(WaddleTheme.unreadBadge, in: Capsule())
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .padding(.horizontal, 8)
        }
        .buttonStyle(.plain)
    }
}
