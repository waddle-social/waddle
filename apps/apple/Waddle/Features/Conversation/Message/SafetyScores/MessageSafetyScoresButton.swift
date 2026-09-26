import SwiftUI
import WaddleKit

/// The marker on a row whose scores crossed the notice threshold. Every
/// participant sees it; it opens the per-category breakdown. It is an
/// annotation, not a moderation action, so it stays small and reflects
/// the strongest visible safety category.
struct MessageSafetyScoresButton: View {
    static let title = "Content signals"
    @Environment(\.colorScheme) private var colorScheme

    let row: SafetyScoreRow
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: MessageSafetyScoreStyle.symbol(for: row.category))
                .font(.caption2)
                .foregroundStyle(MessageSafetyScoreStyle.color(for: row.category, scheme: colorScheme))
                .padding(Theme.Spacing.xs)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("\(Self.title): \(row.title) \(row.percentText)")
        // The row is one combined accessibility element; VoiceOver reaches
        // the breakdown through the row's custom action instead.
        .accessibilityHidden(true)
    }
}

/// Category identity shared by the message marker and score breakdown.
enum MessageSafetyScoreStyle {
    static func symbol(for category: SafetyCategory) -> String {
        switch category {
        case .isQuestion: "questionmark.circle"
        case .hateSpeech: "person.crop.circle.badge.xmark"
        case .explicit: "eye.slash"
        case .harassment: "exclamationmark.bubble"
        case .violence: "bolt"
        case .selfHarm: "heart"
        case .spam: "tray"
        case .scam: "exclamationmark.shield"
        }
    }

    static func color(for category: SafetyCategory, scheme: ColorScheme) -> Color {
        let hex: UInt32 = switch (category, scheme) {
        case (.isQuestion, .light): 0x2563EB
        case (.isQuestion, .dark): 0x60A5FA
        case (.hateSpeech, .light): 0xDC2626
        case (.hateSpeech, .dark): 0xF87171
        case (.explicit, .light): 0x7C3AED
        case (.explicit, .dark): 0xA78BFA
        case (.harassment, .light): 0xC2410C
        case (.harassment, .dark): 0xFB923C
        case (.violence, .light): 0xBE123C
        case (.violence, .dark): 0xFB7185
        case (.selfHarm, .light): 0x0F766E
        case (.selfHarm, .dark): 0x2DD4BF
        case (.spam, .light): 0x475569
        case (.spam, .dark): 0xCBD5E1
        case (.scam, .light): 0xB45309
        case (.scam, .dark): 0xFBBF24
        @unknown default: 0x475569
        }
        return Color(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: 1
        )
    }
}
