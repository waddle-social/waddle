import SwiftUI
import WaddleKit

/// One XEP-0004 field of a command form. Blocked and hidden fields are
/// filtered out before a row is made; `fixed` fields are read-only text.
struct ExtensionCommandFieldRow: View {
    let field: ExtensionCommandField
    /// False for a finished command's result form.
    let isEditable: Bool
    let onChange: ([String]) -> Void

    var body: some View {
        switch field.type {
        case .fixed:
            fixedText
        case .boolean:
            Toggle(title, isOn: booleanValue)
                .disabled(!isEditable)
        case .listSingle:
            Picker(title, selection: singleValue) {
                Text("Choose…").tag("")
                ForEach(field.options, id: \.value) { option in
                    Text(option.label ?? option.value).tag(option.value)
                }
            }
            .disabled(!isEditable)
        case .listMulti:
            multiSelect
        case .textMulti, .jidMulti:
            multiLine
        case .textPrivate:
            SecureField(title, text: singleValue)
                .disabled(!isEditable)
        case .jidSingle:
            TextField(title, text: singleValue)
                .autocorrectionDisabled()
                #if os(iOS)
                .textInputAutocapitalization(.never)
                .keyboardType(.emailAddress)
                #endif
                .disabled(!isEditable)
        case .textSingle:
            TextField(title, text: singleValue)
                .disabled(!isEditable)
        case .hidden:
            EmptyView()
        }
    }

    private var title: String {
        ExtensionCommandCopy.label(for: field)
    }

    private var fixedText: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            if let label = field.label, !label.isEmpty {
                Text(label)
                    .font(.subheadline.weight(.semibold))
            }
            Text(field.values.joined(separator: "\n"))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
    }

    private var multiSelect: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text(title)
                .font(.subheadline)
            ForEach(field.options, id: \.value) { option in
                Toggle(option.label ?? option.value, isOn: optionValue(option.value))
            }
        }
        .disabled(!isEditable)
    }

    private var multiLine: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text(title)
                .font(.subheadline)
            TextEditor(text: linesValue)
                .font(.body)
                .frame(minHeight: 80)
                .autocorrectionDisabled(field.type == .jidMulti)
            if field.type == .jidMulti {
                Text("One address per line")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .disabled(!isEditable)
    }

    // MARK: - Bindings

    private var singleValue: Binding<String> {
        Binding(
            get: { field.values.first ?? "" },
            set: { onChange($0.isEmpty ? [] : [$0]) }
        )
    }

    /// XEP-0004 booleans accept `1`/`true`; the row writes `1` or `0`.
    private var booleanValue: Binding<Bool> {
        Binding(
            get: { ["1", "true"].contains(field.values.first?.lowercased() ?? "") },
            set: { onChange([$0 ? "1" : "0"]) }
        )
    }

    private var linesValue: Binding<String> {
        Binding(
            get: { field.values.joined(separator: "\n") },
            set: { onChange($0.isEmpty ? [] : $0.components(separatedBy: "\n")) }
        )
    }

    /// A list-multi option's toggle; values keep the options' order.
    private func optionValue(_ value: String) -> Binding<Bool> {
        Binding(
            get: { field.values.contains(value) },
            set: { isOn in
                let selected = field.options.map(\.value).filter { option in
                    option == value ? isOn : field.values.contains(option)
                }
                onChange(selected)
            }
        )
    }
}
