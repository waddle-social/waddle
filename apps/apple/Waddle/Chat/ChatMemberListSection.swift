import SwiftUI

struct ChatMemberListSection: View {
    let members: [ChatRoomMember]
    var title: String = "Members"

    var body: some View {
        GroupBox(title) {
            if members.isEmpty {
                Text("No members loaded yet.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(members) { member in
                        HStack(spacing: 10) {
                            memberBadge(for: member)

                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(member.displayName)
                                    if member.isSelf {
                                        Text("you")
                                            .font(.caption2.weight(.semibold))
                                            .foregroundStyle(.secondary)
                                            .padding(.horizontal, 5)
                                            .padding(.vertical, 2)
                                            .background(.quaternary.opacity(0.5), in: Capsule())
                                    }
                                }
                                .font(.subheadline.weight(.medium))

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
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func memberBadge(for member: ChatRoomMember) -> some View {
        Text(member.avatarInitials ?? initials(from: member.displayName))
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .frame(width: 30, height: 30)
            .background(presenceColor(for: member.presence).opacity(0.18), in: Circle())
            .overlay(alignment: .bottomTrailing) {
                Circle()
                    .fill(presenceColor(for: member.presence))
                    .frame(width: 9, height: 9)
                    .offset(x: 1, y: 1)
            }
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }

    private func presenceColor(for state: ChatPresenceState) -> Color {
        switch state {
        case .available: return WaddleTheme.presenceOnline
        case .away:      return WaddleTheme.presenceAway
        case .dnd:       return WaddleTheme.presenceDnd
        case .offline, .unknown: return WaddleTheme.presenceOffline
        }
    }
}

struct ChatTypingIndicatorView: View {
    let typingUsers: [String]
    @State private var animate = false

    var body: some View {
        if !typingUsers.isEmpty {
            HStack(spacing: 8) {
                // Three small dots with a staggered pulse — same motif Slack
                // and Discord use, and cheaper to render than a ProgressView.
                HStack(spacing: 3) {
                    ForEach(0..<3, id: \.self) { index in
                        Circle()
                            .fill(WaddleTheme.textMuted)
                            .frame(width: 5, height: 5)
                            .opacity(animate ? 1.0 : 0.35)
                            .animation(
                                .easeInOut(duration: 0.55)
                                    .repeatForever()
                                    .delay(Double(index) * 0.14),
                                value: animate
                            )
                    }
                }
                Text(typingLabel)
                    .font(.caption2)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
            .onAppear { animate = true }
            .onDisappear { animate = false }
        }
    }

    private var typingLabel: String {
        switch typingUsers.count {
        case 1:
            return "\(typingUsers[0]) is typing…"
        case 2:
            return "\(typingUsers[0]) and \(typingUsers[1]) are typing…"
        default:
            return "\(typingUsers[0]) and \(typingUsers.count - 1) others are typing…"
        }
    }
}
