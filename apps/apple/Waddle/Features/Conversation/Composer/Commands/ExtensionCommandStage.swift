import Foundation
import WaddleKit

/// The command response shown in the form sheet. Its id stays the same
/// across stages so the sheet stays up while the command advances.
struct ExtensionCommandStage: Identifiable {
    let id = UUID()
    let command: ExtensionCommand
    /// The room the command runs in; nil in a 1:1 conversation.
    let room: BareJID?
    var result: ExtensionCommandResult

    /// The command awaits the next action (with or without fields to
    /// fill); otherwise this is a finished command's result form, shown
    /// read-only.
    var isPending: Bool { result.awaitsAction }

    /// Fields the sheet renders: never blocked (secret) or hidden ones.
    var visibleFields: [ExtensionCommandField] {
        Self.visibleFields(of: result.form)
    }

    /// Forward and back actions the stage allows, in button order. With no
    /// `<actions/>` the only way forward is `complete` (XEP-0050 "Command
    /// Actions"; the client already adds it, this is a fallback);
    /// cancel lives in the toolbar.
    var stepActions: [ExtensionCommandAction] {
        guard isPending else { return [] }
        let allowed: [ExtensionCommandAction] = result.actions.isEmpty ? [.complete] : result.actions
        let order: [ExtensionCommandAction] = [.prev, .next, .execute, .complete]
        return order.filter { allowed.contains($0) }
    }

    /// Whether a response needs the sheet: a stage awaiting an action
    /// (even one with only notes and buttons), or a completed command
    /// whose form carries results to read.
    static func needsSheet(_ result: ExtensionCommandResult) -> Bool {
        if result.awaitsAction { return true }
        return result.status == .completed && !visibleFields(of: result.form).isEmpty
    }

    private static func visibleFields(of form: ExtensionCommandForm?) -> [ExtensionCommandField] {
        (form?.fields ?? []).filter { !$0.blocked && $0.type != .hidden }
    }
}
