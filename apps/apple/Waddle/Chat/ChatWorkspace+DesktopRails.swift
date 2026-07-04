import SwiftUI

// MARK: - Desktop Channel & Collapsed Rails

private struct SpaceActionButton: View {
    let systemName: String
    let accessibilityLabel: String
    let action: () -> Void
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(WaddleTheme.textPrimary)
                .frame(width: 32, height: 32)
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(isHovering ? WaddleTheme.surfaceHover : WaddleTheme.surfaceRaised)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }
        }
        .buttonStyle(.plain)
        .help(accessibilityLabel)
        .accessibilityLabel(accessibilityLabel)
        .onHover { hovering in
            isHovering = hovering
        }
    }
}

private struct ChatDesktopChannelRowView: View {
    let room: ChatRoomSelection
    let isSelected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: room.subtitle == nil ? "number" : "bubble.left.and.text.bubble.right")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(isSelected ? WaddleTheme.accent : WaddleTheme.textMuted)
                    .frame(width: 16)

                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Text(room.title)
                            .font(.system(size: 14, weight: isSelected ? .semibold : .medium))
                            .foregroundStyle(WaddleTheme.textPrimary)
                            .lineLimit(1)

                        if room.unreadCount > 0 {
                            Text("\(room.unreadCount)")
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(Color.white)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(WaddleTheme.unreadBadge, in: Capsule())
                        }
                    }

                    if let subtitle = room.subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .font(.caption)
                            .foregroundStyle(WaddleTheme.textSecondary)
                            .lineLimit(1)
                    }
                }

                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(rowBackground)
        }
        .buttonStyle(.plain)
    }

    private var rowBackground: some View {
        RoundedRectangle(cornerRadius: 14, style: .continuous)
            .fill(isSelected ? WaddleTheme.channelSelected : Color.clear)
            .overlay {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(isSelected ? Color.accentColor.opacity(0.20) : Color.clear)
            }
    }
}

extension WaddleChatSpaceView {
    var desktopChannelRail: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: 12) {
                WaddleBrandMark(size: 32)
                    .frame(width: 38, height: 38)
                    .background(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .fill(WaddleTheme.surfaceRaised)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                    }

                VStack(alignment: .leading, spacing: 3) {
                    Text(space.name)
                        .font(.system(size: 17, weight: .semibold))
                        .lineLimit(1)
                    Text(serverLabel)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .lineLimit(1)
                }

                Spacer(minLength: 0)

                SpaceActionButton(systemName: "plus", accessibilityLabel: "Create channel") {
                    showCreateChannelSheet = true
                }

                SpaceActionButton(systemName: "gearshape", accessibilityLabel: "Space settings") {
                    editSpaceName = space.name
                    editSpaceDescription = space.description ?? ""
                    showSpaceSettingsSheet = true
                }

                SpaceActionButton(systemName: "sidebar.leading", accessibilityLabel: "Collapse channels") {
                    withAnimation(.easeOut(duration: 0.18)) {
                        showDesktopChannelRail = false
                    }
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 16)

            Divider()
                .overlay(WaddleTheme.divider)

            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .center, spacing: 10) {
                    Text("Channels")
                        .font(.system(size: 12, weight: .bold))
                        .tracking(1.4)
                        .foregroundStyle(WaddleTheme.textMuted)
                        .textCase(.uppercase)

                    Spacer()

                    Text("Member")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(WaddleTheme.presenceOnline)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 4)
                        .background(WaddleTheme.presenceOnline.opacity(0.10), in: Capsule())
                }

                Text("\(store.rooms.count) channels · \(model.members.count) people")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(WaddleTheme.textSecondary)
                    .lineLimit(1)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)

            if store.rooms.isEmpty {
                ChatEmptyStateView(
                    title: "No channels yet",
                    message: "Channels will appear here once the server space has rooms.",
                    systemImage: "number"
                )
                .padding(.horizontal, 18)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(channelGroups) { group in
                            if model.spaces.count > 1 {
                                Text(group.space.name)
                                    .font(.system(size: 11, weight: .semibold))
                                    .foregroundStyle(WaddleTheme.textMuted)
                                    .textCase(.uppercase)
                                    .padding(.horizontal, 12)
                                    .padding(.top, 10)
                                    .padding(.bottom, 4)
                            }

                            ForEach(group.rooms) { room in
                                ChatDesktopChannelRowView(
                                    room: room,
                                    isSelected: model.selectedChannelID == room.id
                                ) {
                                    Task { await model.selectChannel(room.id) }
                                }
                                .contextMenu {
                                    Button {
                                        editingChannelID = room.id
                                        editChannelName = room.title
                                        editChannelDescription = model.channels.first(where: { $0.id == room.id })?.description ?? ""
                                        Task { await model.selectChannel(room.id) }
                                        showEditChannelSheet = true
                                    } label: {
                                        Label("Edit Channel", systemImage: "pencil")
                                    }
                                }
                            }
                        }

                        if !store.dmConversations.isEmpty {
                            Text("Direct messages")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundStyle(WaddleTheme.textMuted)
                                .textCase(.uppercase)
                                .padding(.horizontal, 12)
                                .padding(.top, 18)
                                .padding(.bottom, 6)

                            ForEach(store.dmConversations) { convo in
                                Button {
                                    Task { await model.openDm(peerJID: convo.peerJID, peerUsername: convo.peerUsername) }
                                } label: {
                                    HStack(spacing: 10) {
                                        Circle()
                                            .fill(convo.presenceShow == .available ? WaddleTheme.presenceOnline : WaddleTheme.presenceOffline)
                                            .frame(width: 8, height: 8)
                                        Text(convo.peerUsername)
                                            .font(.system(size: 14, weight: .medium))
                                            .foregroundStyle(WaddleTheme.textPrimary)
                                            .lineLimit(1)
                                        Spacer()
                                        if convo.unreadCount > 0 {
                                            Text("\(convo.unreadCount)")
                                                .font(.caption2.weight(.bold))
                                                .foregroundStyle(Color.white)
                                                .padding(.horizontal, 6)
                                                .padding(.vertical, 2)
                                                .background(WaddleTheme.unreadBadge, in: Capsule())
                                        }
                                    }
                                    .padding(.horizontal, 12)
                                    .padding(.vertical, 8)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                }
            }

            Divider()
                .overlay(WaddleTheme.divider)

            Button {
                if showDesktopMemberPane {
                    withAnimation(.easeOut(duration: 0.18)) {
                        showDesktopMemberPane = false
                    }
                } else if model.chatMembers.isEmpty {
                    showMembersSheet = true
                } else {
                    withAnimation(.easeOut(duration: 0.18)) {
                        showDesktopMemberPane = true
                    }
                }
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: "person.2")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                    Text("Members")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(WaddleTheme.textPrimary)
                    Spacer()
                    Text("\(model.chatMembers.count)")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 16)
            }
            .buttonStyle(.plain)
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(WaddleTheme.sidebarBackground)
    }

    var desktopCollapsedChannelRail: some View {
        VStack(spacing: 12) {
            Button {
                withAnimation(.easeOut(duration: 0.18)) {
                    showDesktopChannelRail = true
                }
            } label: {
                Image(systemName: "sidebar.left")
                    .font(.caption.weight(.semibold))
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)

            Text("\(store.rooms.count)")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .frame(width: 28, height: 18)
                .background(WaddleTheme.surfaceRaised, in: Capsule())
                .overlay {
                    Capsule()
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }

            Spacer(minLength: 0)
        }
        .padding(.vertical, 12)
        .frame(maxHeight: .infinity)
        .background(WaddleTheme.sidebarBackground)
    }

    var desktopCollapsedMemberRail: some View {
        VStack(spacing: 12) {
            Button {
                withAnimation(.easeOut(duration: 0.18)) {
                    showDesktopMemberPane = true
                }
            } label: {
                Image(systemName: "person.2")
                    .font(.caption.weight(.semibold))
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)

            Text("\(model.chatMembers.count)")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .frame(width: 28, height: 18)
                .background(WaddleTheme.surfaceRaised, in: Capsule())
                .overlay {
                    Capsule()
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }

            Spacer(minLength: 0)
        }
        .padding(.vertical, 12)
        .frame(maxHeight: .infinity)
        .background(WaddleTheme.sidebarBackground)
    }
}
