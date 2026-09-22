import SwiftUI
import WaddleKit

/// Pick an XEP-0107 mood and an optional note, or clear it.
struct ProfileMoodEditor: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var selection: String?
    @State private var note: String
    @State private var isSaving = false
    @State private var errorMessage: String?
    private let current: UserMood?

    init(current: UserMood?) {
        self.current = current
        _selection = State(initialValue: current?.value)
        _note = State(initialValue: current?.text ?? "")
    }

    var body: some View {
        Form {
            Section("How are you feeling?") {
                ProfileMoodGrid(options: ProfileMoodCatalog.options(including: current?.value), selection: $selection)
            }
            Section {
                TextField("Note", text: $note, prompt: Text("Add a note (optional)"))
                    .submitLabel(.done)
                    .onSubmit(save)
            } footer: {
                Text("Your mood is shared with your contacts.")
            }
            if let errorMessage {
                Section {
                    Text(errorMessage)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
            if current != nil {
                Section {
                    Button("Clear mood", role: .destructive, action: clear)
                        .disabled(isSaving)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Mood")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if isSaving {
                    ProgressView().controlSize(.small)
                } else {
                    Button("Save", action: save)
                        .disabled(pendingMood == nil)
                }
            }
        }
    }

    private var pendingMood: UserMood? {
        selection.flatMap { ProfileMoodCatalog.mood(value: $0, note: note) }
    }

    private func save() {
        guard let mood = pendingMood else { return }
        perform { try await session.setMood(mood) }
    }

    private func clear() {
        perform { try await session.setMood(nil) }
    }

    private func perform(_ work: @escaping () async throws -> Void) {
        guard !isSaving else { return }
        isSaving = true
        errorMessage = nil
        Task {
            do {
                try await work()
                isSaving = false
                dismiss()
            } catch {
                isSaving = false
                errorMessage = error.localizedDescription
            }
        }
    }
}

/// Emoji tiles for the mood options.
private struct ProfileMoodGrid: View {
    let options: [ProfileMoodOption]
    @Binding var selection: String?

    private let columns = [GridItem(.adaptive(minimum: 76), spacing: Theme.Spacing.s)]

    var body: some View {
        LazyVGrid(columns: columns, spacing: Theme.Spacing.s) {
            ForEach(options) { option in
                ProfileMoodTile(option: option, isSelected: selection == option.value) {
                    selection = option.value
                }
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .sensoryFeedback(.selection, trigger: selection)
    }
}

private struct ProfileMoodTile: View {
    let option: ProfileMoodOption
    let isSelected: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            VStack(spacing: Theme.Spacing.xs) {
                Text(option.emoji)
                    .font(.title)
                Text(option.title)
                    .font(.caption)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
            .frame(maxWidth: .infinity, minHeight: 64)
            .background(background)
            .contentShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(option.title))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var background: some View {
        RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
            .fill(isSelected ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.08))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .strokeBorder(isSelected ? Color.accentColor : Color.clear, lineWidth: 1.5)
            )
    }
}
