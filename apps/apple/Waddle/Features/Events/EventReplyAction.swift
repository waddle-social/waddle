import SwiftUI
import WaddleKit

/// Sends the user's answer and keeps its failure with the screen that presents it.
@MainActor @Observable
final class EventReplyAction {
    enum Feedback {
        case alert, inline
    }

    private let feedback: Feedback
    private var errorMessage: String?

    init(feedback: Feedback) {
        self.feedback = feedback
    }

    func selection(for eventID: String, in session: SessionCoordinator) -> Binding<EventReply?> {
        Binding(
            get: { session.events.myReply(to: eventID) },
            set: { reply in
                guard let reply else { return }
                if self.feedback == .inline { self.errorMessage = nil }
                Task {
                    do {
                        try await session.reply(reply, to: eventID)
                    } catch {
                        let fallback = self.feedback == .inline ? "Couldn't send your answer. Try again." : "Try again."
                        self.errorMessage = ActionErrorCopy.message(for: error, fallback: fallback)
                    }
                }
            }
        )
    }

    @ViewBuilder var inlineFailure: some View {
        if let errorMessage {
            Label(errorMessage, systemImage: "exclamationmark.triangle")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.dangerText)
        }
    }

    func presentingAlert<Content: View>(on content: Content) -> some View {
        let message = errorMessage
        return content.alert("Couldn't send your answer", isPresented: Binding(
            get: { self.errorMessage != nil },
            set: { if !$0 { self.errorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { self.errorMessage = nil }
        } message: {
            Text(message ?? "")
        }
    }
}

/// A reply can hide its card while it is being sent in the Going or maybe filter.
struct EventReplyAlert<Content: View>: View {
    @State private var reply = EventReplyAction(feedback: .alert)
    @ViewBuilder let content: (EventReplyAction) -> Content

    var body: some View {
        reply.presentingAlert(on: content(reply))
    }
}
