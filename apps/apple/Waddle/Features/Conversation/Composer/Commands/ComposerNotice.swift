import Foundation
import WaddleKit

/// A transient line above the composer: a command's notes or outcome.
struct ComposerNotice: Identifiable, Equatable {
    enum Severity: Equatable {
        case info
        case warning
        case error
    }

    let id = UUID()
    let severity: Severity
    let text: String

    /// XEP-0050 notes as one notice with the most severe note's level.
    static func from(notes: [ExtensionCommandNote]) -> ComposerNotice? {
        guard !notes.isEmpty else { return nil }
        let severity: Severity
        if notes.contains(where: { $0.type == .error }) {
            severity = .error
        } else if notes.contains(where: { $0.type == .warn }) {
            severity = .warning
        } else {
            severity = .info
        }
        return ComposerNotice(severity: severity, text: notes.map(\.text).joined(separator: "\n"))
    }

    /// What a finished command reports: its notes, else a short outcome.
    static func outcome(of result: ExtensionCommandResult, command: ExtensionCommand) -> ComposerNotice? {
        if let notes = from(notes: result.notes) { return notes }
        switch result.status {
        case .completed: return ComposerNotice(severity: .info, text: "\(command.name) finished.")
        case .canceled, .executing: return nil
        }
    }
}
