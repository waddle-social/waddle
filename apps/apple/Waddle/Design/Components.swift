import SwiftUI
import WaddleKit

/// XEP-0084 avatar with an initials fallback on a name-derived tint.
struct AvatarView: View {
    let name: String
    var image: AvatarImage?
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        Group {
            if let image, let decoded = Image(data: image.data) {
                decoded
                    .resizable()
                    .scaledToFill()
            } else {
                ZStack {
                    Self.tint(for: name)
                    Text(initials)
                        .font(HuddleFonts.swiftUIFont(.body, size: size * 0.36, weight: 700))
                        .foregroundStyle(Theme.Colors.avatarFg)
                }
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .accessibilityHidden(true)
    }

    /// The fallback tint for `name`: the web client's djb2 over code
    /// points (unsigned 32-bit) modulo the avatar tints, so a person keeps
    /// one colour on web, Android and Apple.
    static func tint(for name: String) -> Color {
        let tints = Theme.Colors.avatarTints
        return tints[tintIndex(for: name, count: tints.count)]
    }

    static func tintIndex(for name: String, count: Int) -> Int {
        let hash = name.unicodeScalars.reduce(UInt32(5381)) { hash, scalar in
            hash &* 33 &+ scalar.value
        }
        return Int(hash % UInt32(count))
    }

    private var initials: String {
        let parts = name.split(whereSeparator: { $0 == " " || $0 == "." || $0 == "_" || $0 == "-" }).prefix(2)
        let letters = parts.compactMap(\.first).map(String.init).joined()
        return letters.isEmpty ? "?" : letters.uppercased()
    }
}

/// Avatar for a JID, fetched through the session's avatar store. While
/// on screen it asks again each minute and after every reconnect; the
/// store decides whether a lookup is due.
struct JIDAvatar: View {
    @Environment(SessionCoordinator.self) private var session
    let jid: BareJID
    var name: String?
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        AvatarView(
            name: name ?? jid.localpart ?? jid.domain,
            image: session.avatars.image(for: jid),
            size: size
        )
        .task(id: RequestKey(jid: jid, generation: session.avatars.generation)) {
            while !Task.isCancelled {
                session.loadAvatarIfNeeded(jid)
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }

    private struct RequestKey: Hashable {
        let jid: BareJID
        let generation: Int
    }
}

/// RFC 6121 availability dot. Every state has its own shape, never
/// colour alone: filled when here, half-filled when away, barred for do
/// not disturb, hollow when offline.
struct PresenceDot: View {
    let availability: Availability
    var size: CGFloat = 10
    /// The colour the dot sits on; its ring and cut-outs are drawn in it.
    var ground: Color = Theme.Colors.bg

    var body: some View {
        mark
            .frame(width: size, height: size)
            .overlay(Circle().strokeBorder(ground, lineWidth: ringWidth))
            .accessibilityLabel(Text(label))
    }

    private var ringWidth: CGFloat { size * 0.14 }
    private var strokeWidth: CGFloat { max(1.5, size * 0.14) }

    @ViewBuilder
    private var mark: some View {
        let inner = Circle().inset(by: ringWidth)
        switch availability {
        case .available, .chat:
            Circle().fill(color)
        case .away, .extendedAway:
            ZStack {
                Circle().fill(ground)
                // The leading half, as on the web.
                inner.fill(color).mask(alignment: .leading) {
                    Rectangle().frame(width: size / 2)
                }
                inner.strokeBorder(color, lineWidth: strokeWidth)
            }
        case .doNotDisturb:
            ZStack {
                Circle().fill(color)
                Capsule().fill(ground).frame(width: size * 0.42, height: strokeWidth)
            }
        case .offline:
            ZStack {
                Circle().fill(ground)
                inner.strokeBorder(color, lineWidth: strokeWidth)
            }
        }
    }

    private var color: Color {
        switch availability {
        case .available, .chat: return Theme.Colors.presenceAvailable
        case .away, .extendedAway: return Theme.Colors.presenceAway
        case .doNotDisturb: return Theme.Colors.presenceDnd
        case .offline: return Theme.Colors.presenceOffline
        }
    }

    private var label: String {
        switch availability {
        case .available, .chat: return "Online"
        case .away: return "Away"
        case .extendedAway: return "Away for a while"
        case .doNotDisturb: return "Do not disturb"
        case .offline: return "Offline"
        }
    }
}

/// Unread count pill. Gold means "this is for you", so only the mention
/// variant is gold; plain unread is quiet.
struct UnreadBadge: View {
    let count: Int
    var isMention = false

    private static var height: CGFloat { 20 }

    var body: some View {
        if count > 0 {
            Text(count > 99 ? "99+" : "\(count)")
                .huddleText(Theme.Typography.badge)
                .monospacedDigit()
                .foregroundStyle(isMention ? Theme.Colors.goldFg : Theme.Colors.fgMuted)
                .padding(.horizontal, WaddleTokens.Spacing.s1_5)
                .frame(minWidth: Self.height, minHeight: Self.height)
                .background(Capsule().fill(isMention ? Theme.Colors.gold : Theme.Colors.surface2))
                .overlay {
                    if !isMention {
                        Capsule().strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
                    }
                }
                .accessibilityLabel(Text(isMention ? "\(count) unread, mentions you" : "\(count) unread"))
        }
    }
}

/// Offline / reconnecting strip shown above the content.
struct ConnectionBanner: View {
    let status: ConnectionStatus

    var body: some View {
        if let content {
            HStack(spacing: Theme.Spacing.s) {
                if content.showsProgress {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: content.symbol)
                }
                Text(content.text)
                    .huddleText(Theme.Typography.label)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.s)
            .foregroundStyle(content.foreground)
            .background(content.background)
            .transition(.move(edge: .top).combined(with: .opacity))
            .accessibilityElement(children: .combine)
        }
    }

    private struct Content {
        let text: String
        let symbol: String
        let foreground: Color
        let background: Color
        var showsProgress = false
    }

    private var content: Content? {
        typealias Colors = Theme.Colors
        switch status {
        case .online, .signedOut:
            return nil
        case .connecting:
            return Content(
                text: "Connecting…",
                symbol: "arrow.triangle.2.circlepath",
                foreground: Colors.fgMuted,
                background: Colors.surface2,
                showsProgress: true
            )
        case let .offline(retryAt):
            let text = if let retryAt, retryAt > Date() {
                "Offline. Retrying \(retryAt.formatted(.relative(presentation: .named)))"
            } else {
                "Offline. Reconnecting…"
            }
            return Content(
                text: text,
                symbol: "wifi.slash",
                foreground: Colors.warningText,
                background: Colors.warningSubtle
            )
        case .authenticationFailed:
            return Content(
                text: "Your session expired. Sign in again.",
                symbol: "person.crop.circle.badge.exclamationmark",
                foreground: Colors.dangerText,
                background: Colors.dangerSubtle
            )
        }
    }
}

/// Centered empty or error state.
struct EmptyStateView: View {
    let title: String
    let message: String
    let symbol: String

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            Image(systemName: symbol)
                .font(.system(size: 40, weight: .regular))
                .foregroundStyle(Theme.Colors.fgSoft)
                .padding(.bottom, Theme.Spacing.xs)
                .accessibilityHidden(true)
            Text(title)
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
            Text(message)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .multilineTextAlignment(.center)
        .padding(Theme.Spacing.xl)
        .frame(maxWidth: 420)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}

struct WaddleBrandMark: View {
    var size: CGFloat = 44

    var body: some View {
        Image("WaddleLogo")
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
