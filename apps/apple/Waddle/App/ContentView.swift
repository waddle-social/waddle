import SwiftUI

struct ContentView: View {
    @StateObject private var model = AppModel()
    @State private var showCreateSheet = false

    var body: some View {
        Group {
            if model.session == nil {
                SignInView(model: model)
            } else {
                NavigationSplitView {
                    WaddleListView(model: model, showCreateSheet: $showCreateSheet)
                } detail: {
                    WaddleDetailView(model: model)
                }
                .sheet(isPresented: $showCreateSheet) {
                    CreateWaddleSheet(model: model)
                }
            }
        }
    }
}

private struct SignInView: View {
    @ObservedObject var model: AppModel
    @Environment(\.openURL) private var openURL

    var body: some View {
        VStack(spacing: 18) {
            VStack(spacing: 6) {
                Text("Waddle")
                    .font(.largeTitle.weight(.bold))
                Text("Native SwiftUI client")
                    .foregroundStyle(.secondary)
            }

            GroupBox("Server") {
                HStack(spacing: 8) {
                    TextField("https://xmpp.waddle.social", text: $model.serverURLText)
#if os(iOS)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
#endif
                        .textFieldStyle(.roundedBorder)

                    Button("Apply") {
                        Task { await model.applyServerURL() }
                    }
                    .buttonStyle(.borderedProminent)
                }
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
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(model.providers) { provider in
                            Button("Continue with \(provider.displayName ?? provider.id)") {
                                Task { await model.startDeviceAuthorization(provider: provider, openURL: openURL) }
                            }
                            .buttonStyle(.borderedProminent)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            if let flow = model.deviceAuth {
                GroupBox("Device authorization") {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Code")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(flow.userCode)
                            .font(.title2.monospaced())
                            .fontWeight(.semibold)

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
        .padding(24)
        .frame(maxWidth: 680)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }
}

private struct WaddleListView: View {
    @ObservedObject var model: AppModel
    @Binding var showCreateSheet: Bool

    var body: some View {
        List(selection: $model.selectedWaddleID) {
            Section {
                ForEach(model.publicWaddles) { waddle in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(waddle.name)
                                .font(.body.weight(.semibold))
                            if let description = waddle.description, !description.isEmpty {
                                Text(description)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                        }
                        Spacer()
                        if model.isJoined(waddle.id) {
                            Text("Joined")
                                .font(.caption.weight(.semibold))
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(.green.opacity(0.18), in: Capsule())
                        }
                    }
                    .tag(waddle.id)
                }
            } header: {
                Text("Public Waddles")
            }
        }
        .navigationTitle("Waddles")
        .searchable(text: $model.searchQuery, placement: .sidebar, prompt: "Search public waddles")
        .onChange(of: model.searchQuery) { _, _ in
            model.schedulePublicWaddleSearch()
        }
        .toolbar {
            ToolbarItemGroup {
                if model.isLoadingWaddles {
                    ProgressView()
                }
                Button {
                    Task { await model.refreshPublicWaddles() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                Button {
                    showCreateSheet = true
                } label: {
                    Image(systemName: "plus")
                }
            }
            ToolbarItem(placement: .automatic) {
                Button("Sign out") {
                    Task { await model.signOut() }
                }
            }
        }
        .task {
            if model.publicWaddles.isEmpty {
                await model.refreshPublicWaddles()
            }
        }
    }
}

private struct WaddleDetailView: View {
    @ObservedObject var model: AppModel

    private var selectedWaddle: WaddleSummary? {
        guard let selectedWaddleID = model.selectedWaddleID else { return nil }
        return model.publicWaddles.first(where: { $0.id == selectedWaddleID })
    }

    var body: some View {
        Group {
            if let waddle = selectedWaddle {
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        Text(waddle.name)
                            .font(.title.bold())

                        if let description = waddle.description, !description.isEmpty {
                            Text(description)
                                .foregroundStyle(.secondary)
                        }

                        HStack(spacing: 10) {
                            if model.isJoined(waddle.id) {
                                Label("Joined", systemImage: "checkmark.circle.fill")
                                    .foregroundStyle(.green)
                            } else {
                                Button("Join waddle") {
                                    Task { await model.join(waddle) }
                                }
                                .buttonStyle(.borderedProminent)
                            }
                        }

                        Divider()

                        Text("This is a native SwiftUI shell for Waddle with native auth and community browsing.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .padding(24)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ContentUnavailableView("No waddle selected", systemImage: "bubble.left.and.bubble.right")
            }
        }
        .navigationTitle(selectedWaddle?.name ?? "Waddle")
        .overlay(alignment: .bottomLeading) {
            if !model.errorMessage.isEmpty {
                Text(model.errorMessage)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .padding()
            }
        }
    }
}

private struct CreateWaddleSheet: View {
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
