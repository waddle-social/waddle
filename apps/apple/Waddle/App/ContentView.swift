import SwiftUI

struct ContentView: View {
    @StateObject private var model = AppModel()
    @State private var showCreateSheet = false
    @State private var showSettings = false
    @AppStorage(AppConfig.themePreferenceKey) private var themePreferenceRaw = AppThemePreference.system.rawValue
    @Environment(\.scenePhase) private var scenePhase
#if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
#endif

    private var themePreference: AppThemePreference {
        AppThemePreference(rawValue: themePreferenceRaw) ?? .system
    }

    var body: some View {
        rootContent
            .preferredColorScheme(themePreference.preferredColorScheme)
            .sheet(isPresented: $showSettings) {
                AppSettingsPanel()
            }
            .onChange(of: scenePhase) { _, newPhase in
                if newPhase == .active {
                    model.handleAppBecameActive()
                }
            }
    }

    @ViewBuilder
    private var rootContent: some View {
        if model.session == nil {
            SignInView(model: model) {
                showSettings = true
            }
        } else {
            authenticatedShell
        }
    }

    @ViewBuilder
    private var authenticatedShell: some View {
#if os(iOS)
        if horizontalSizeClass == .compact {
            MobileSlackShellView(model: model, showCreateSheet: $showCreateSheet)
                .sheet(isPresented: $showCreateSheet) {
                    CreateWaddleSheet(model: model)
                }
        } else {
            desktopAuthenticatedShell
        }
#else
        desktopAuthenticatedShell
#endif
    }

    private var desktopAuthenticatedShell: some View {
#if os(macOS)
        DesktopAuthenticatedShell(
            model: model,
            showCreateSheet: $showCreateSheet
        ) {
            showSettings = true
        }
#else
        NavigationSplitView {
            WaddleListView(
                model: model,
                showCreateSheet: $showCreateSheet
            ) {
                showSettings = true
            }
        } detail: {
            WaddleDetailView(model: model)
        }
        .sheet(isPresented: $showCreateSheet) {
            CreateWaddleSheet(model: model)
        }
#endif
    }
}

private struct SignInView: View {
    @ObservedObject var model: AppModel
    @Environment(\.openURL) private var openURL
    let onShowSettings: () -> Void

    var body: some View {
#if os(iOS)
        ZStack {
            LinearGradient(
                colors: [
                    Color.accentColor.opacity(0.16),
                    Color(.systemBackground),
                    Color(.secondarySystemBackground)
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            .ignoresSafeArea()

            ScrollView {
                content
                    .padding(.horizontal, 20)
                    .padding(.top, 24)
                    .padding(.bottom, 40)
                    .frame(maxWidth: 560)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
#else
        content
            .padding(24)
            .frame(maxWidth: 680)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
#endif
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                Spacer()
                Button {
                    onShowSettings()
                } label: {
                    Label("Settings", systemImage: "gearshape")
                }
                .buttonStyle(.bordered)
#if os(macOS)
                .keyboardShortcut(",", modifiers: .command)
#endif
            }

            HStack(alignment: .center, spacing: 16) {
                WaddleBrandMark(size: 68)

                VStack(alignment: .leading, spacing: 10) {
                    Text("Waddle")
                        .font(.system(size: 34, weight: .bold, design: .rounded))

                    Text("Calm, native chat that follows your system theme and keeps longer conversations easy to read.")
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            GroupBox("Server") {
#if os(iOS)
                VStack(alignment: .leading, spacing: 12) {
                    TextField("https://xmpp.waddle.social", text: $model.serverURLText)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .textFieldStyle(.roundedBorder)

                    Button("Apply server") {
                        Task { await model.applyServerURL() }
                    }
                    .buttonStyle(.borderedProminent)
                    .frame(maxWidth: .infinity, alignment: .trailing)
                }
#else
                HStack(spacing: 8) {
                    TextField("https://xmpp.waddle.social", text: $model.serverURLText)
                        .textFieldStyle(.roundedBorder)

                    Button("Apply") {
                        Task { await model.applyServerURL() }
                    }
                    .buttonStyle(.borderedProminent)
                }
#endif
            }

            GroupBox("Sign in") {
                if model.isLoadingProviders {
                    ProgressView()
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else if model.providers.isEmpty {
                    Text("No auth providers available for this server.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(model.providers) { provider in
                            Button("Continue with \(provider.displayName ?? provider.id)") {
                                Task { await model.startDeviceAuthorization(provider: provider, openURL: openURL) }
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            if let flow = model.deviceAuth {
                GroupBox("Device authorization") {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Code")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(flow.userCode)
                            .font(.system(size: 30, weight: .semibold, design: .rounded))

#if os(iOS)
                        VStack(alignment: .leading, spacing: 10) {
                            Button("Open verification page") {
                                model.reopenDeviceVerification(openURL: openURL)
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)

                            Button("Cancel") {
                                model.cancelDeviceAuthorization()
                            }
                            .buttonStyle(.bordered)
                        }
#else
                        HStack {
                            Button("Open verification page") {
                                model.reopenDeviceVerification(openURL: openURL)
                            }
                            .buttonStyle(.borderedProminent)

                            Button("Cancel") {
                                model.cancelDeviceAuthorization()
                            }
                            .buttonStyle(.bordered)
                        }
#endif
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            if !model.errorMessage.isEmpty {
                Text(model.errorMessage)
                    .foregroundStyle(.red)
                    .font(.footnote)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

private struct WaddleListView: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool
    let onShowSettings: () -> Void

    var body: some View {
        List {
            Section {
                if let space = model.selectedWaddle {
                    HStack(alignment: .top, spacing: 12) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(space.name)
                                .font(.body.weight(.semibold))
                            if let description = space.description, !description.isEmpty {
                                Text(description)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                            }
                        }
                        Spacer()
                        Text("Active")
                            .font(.caption.weight(.semibold))
                            .padding(.horizontal, 10)
                            .padding(.vertical, 5)
                            .background(.green.opacity(0.16), in: Capsule())
                    }
                }
            } header: {
                Text("Space")
            }
        }
        .navigationTitle("Waddle")
        .toolbar {
            ToolbarItemGroup {
                if model.isLoadingStructure {
                    ProgressView()
                }
                Button {
                    showCreateSheet = true
                } label: {
                    Image(systemName: "plus")
                }
                Button {
                    onShowSettings()
                } label: {
                    Image(systemName: "gearshape")
                }
#if os(macOS)
                .keyboardShortcut(",", modifiers: .command)
#endif
            }
            ToolbarItem(placement: .automatic) {
                Button("Sign out") {
                    Task { await model.signOut() }
                }
            }
        }
#if os(iOS)
        .listStyle(.insetGrouped)
#endif
    }
}

private struct AppSettingsPanel: View {
    @Environment(\.dismiss) private var dismiss
    @AppStorage(AppConfig.themePreferenceKey) private var themePreferenceRaw = AppThemePreference.system.rawValue
    @AppStorage(AppConfig.scrollDirectionKey) private var scrollDirectionRaw = ChatScrollDirection.chat.rawValue

    var showsDoneButton = true

    var body: some View {
        NavigationStack {
            Form {
                Section("Appearance") {
                    Picker("Theme", selection: $themePreferenceRaw) {
                        ForEach(AppThemePreference.allCases) { preference in
                            Text(preference.title).tag(preference.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)
                }

                Section("Conversation layout") {
                    Picker("Scroll direction", selection: $scrollDirectionRaw) {
                        ForEach(ChatScrollDirection.allCases) { direction in
                            Text(direction.title).tag(direction.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)

                    Text(currentScrollDirection.description)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Settings")
            .toolbar {
                if showsDoneButton {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") {
                            dismiss()
                        }
                    }
                }
            }
        }
#if os(macOS)
        .frame(minWidth: 420, minHeight: 260)
#endif
    }

    private var currentScrollDirection: ChatScrollDirection {
        ChatScrollDirection(rawValue: scrollDirectionRaw) ?? .chat
    }
}

struct WaddleDetailView: View {
    @ObservedObject var model: AppModel

    private var selectedWaddle: WaddleSummary? {
        model.selectedWaddle
    }

    var body: some View {
        Group {
            if let waddle = selectedWaddle {
                WaddleChatWorkspaceView(model: model, store: model.chatStore, waddle: waddle)
            } else {
                ChatEmptyStateView(
                    title: "Pick a waddle",
                    message: "Browse waddles, pick a channel, and your chat view will open here.",
                    systemImage: "bubble.left.and.bubble.right"
                )
            }
        }
        .navigationTitle(selectedWaddle?.name ?? "Waddle")
#if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
#endif
    }
}

struct CreateWaddleSheet: View {
    @ObservedObject var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var description = ""
    @State private var isPublic = true

    var body: some View {
        NavigationStack {
            Form {
                Section("Basics") {
                    TextField("Name", text: $name)
                    TextField("Description", text: $description, axis: .vertical)
                        .lineLimit(3, reservesSpace: true)
                    Toggle("Public", isOn: $isPublic)
                }
            }
            .navigationTitle("Create Waddle")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        Task {
                            await model.createWaddle(name: name, description: description, isPublic: isPublic)
                            if model.errorMessage.isEmpty {
                                dismiss()
                            }
                        }
                    }
                    .disabled(name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isCreatingWaddle)
                }
            }
        }
    }
}
