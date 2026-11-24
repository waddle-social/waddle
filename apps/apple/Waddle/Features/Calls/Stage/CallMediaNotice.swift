import SwiftUI
import WaddleKit

/// The notices over a stage's tiles about our own devices (web
/// `CallMediaNotice`): what the call could not use, a button that tries
/// again, and one that puts the notice away for this call. A status, in
/// the warning tint, not an error: the call goes on without the device.
///
/// Pure: the notices are passed in and a choice is sent out.
struct CallMediaNotice: View {
    let issues: [CallMediaIssue]
    /// The kinds being tried again now: their button says so and waits.
    var checking: Set<CallMediaIssue.Kind> = []
    let retry: @MainActor (CallMediaIssue.Kind) -> Void
    let dismiss: @MainActor (CallMediaIssue.Kind) -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.xs) {
            ForEach(issues) { issue in
                row(issue)
            }
        }
    }

    private func row(_ issue: CallMediaIssue) -> some View {
        // One line where the stage is wide enough for the words and the
        // button; in the narrow column the button goes under the words.
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: WaddleTokens.Spacing.s2Half) {
                icon(issue)
                message(issue).fixedSize()
                Spacer(minLength: Theme.Spacing.s)
                retryButton(issue)
                dismissButton(issue)
            }
            HStack(alignment: .top, spacing: WaddleTokens.Spacing.s2Half) {
                icon(issue)
                VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                    message(issue).fixedSize(horizontal: false, vertical: true)
                    retryButton(issue)
                }
                Spacer(minLength: 0)
                dismissButton(issue)
            }
        }
        .padding(.leading, Theme.Spacing.m)
        .padding([.trailing, .vertical], Theme.Spacing.s)
        .background(Metrics.shape.fill(Theme.Colors.warningText.opacity(0.12)))
        .overlay(Metrics.shape.strokeBorder(Theme.Colors.warningText.opacity(0.35), lineWidth: WaddleTokens.BorderWidth.hair))
        .accessibilityElement(children: .contain)
    }

    private func icon(_ issue: CallMediaIssue) -> some View {
        Image(systemName: Self.symbol(issue.kind))
            .font(.system(size: 14, weight: .medium))
            .foregroundStyle(Theme.Colors.warningText)
            .frame(width: Metrics.icon, height: Metrics.icon)
            .accessibilityHidden(true)
    }

    private func message(_ issue: CallMediaIssue) -> some View {
        Text(issue.message)
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fg)
    }

    private func retryButton(_ issue: CallMediaIssue) -> some View {
        let isChecking = checking.contains(issue.kind)
        return Button(isChecking ? "Checking…" : issue.retryTitle) { retry(issue.kind) }
            .buttonStyle(.huddle(size: .small))
            .fixedSize()
            .disabled(isChecking)
    }

    private func dismissButton(_ issue: CallMediaIssue) -> some View {
        Button { dismiss(issue.kind) } label: {
            Image(systemName: "xmark")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.Colors.fgMuted)
                .frame(width: Metrics.icon, height: Metrics.icon)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Dismiss"))
        .help("Dismiss for this call")
    }

    private static func symbol(_ kind: CallMediaIssue.Kind) -> String {
        switch kind {
        case .microphone: "mic.slash"
        case .camera: "video.slash"
        case .screen: "rectangle.inset.filled.and.person.filled"
        }
    }

    private enum Metrics {
        static let shape = RoundedRectangle(cornerRadius: 12, style: .continuous)
        static let icon: CGFloat = 22
    }
}
