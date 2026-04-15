import Foundation
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    @Published var serverURLText: String
    @Published var providers: [AuthProvider] = []
    @Published var session: WaddleSession?
    @Published var publicWaddles: [WaddleSummary] = []
    @Published var selectedWaddleID: String?
    @Published var searchQuery = ""
    @Published var joinedWaddleIDs: Set<String> = []
    @Published var deviceAuth: DeviceStartResponse?
    @Published var errorMessage = ""
    @Published var isLoadingProviders = false
    @Published var isLoadingWaddles = false
    @Published var isCreatingWaddle = false

    private var serverURL: URL
    private var client: WaddleAPIClient
    private var devicePollTask: Task<Void, Never>?
    private var searchTask: Task<Void, Never>?

    init() {
        let persistedServerURL = AppConfig.persistedServerURL
        self.serverURL = persistedServerURL
        self.serverURLText = persistedServerURL.absoluteString
        self.client = WaddleAPIClient(serverURL: persistedServerURL)
        Task { await bootstrap() }
    }

    func applyServerURL() async {
        guard let next = AppConfig.normalizedServerURL(from: serverURLText) else {
            errorMessage = "Enter a valid server URL."
            return
        }

        if next == serverURL {
            return
        }

        serverURL = next
        serverURLText = next.absoluteString
        client = WaddleAPIClient(serverURL: next)
        AppConfig.saveServerURL(next)
        clearSessionState()
        await bootstrap()
    }

    func bootstrap() async {
        errorMessage = ""
        await loadProviders()

        guard let storedSessionID = AppConfig.storedSessionID(for: serverURL) else {
            return
        }

        do {
            guard let loaded = try await client.session(sessionID: storedSessionID), !loaded.isExpired else {
                AppConfig.clearSessionID(for: serverURL)
                return
            }
            session = loaded
            await refreshPublicWaddles()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func loadProviders() async {
        isLoadingProviders = true
        defer { isLoadingProviders = false }

        do {
            providers = try await client.providers()
        } catch {
            providers = []
            errorMessage = error.localizedDescription
        }
    }

    func startDeviceAuthorization(provider: AuthProvider, openURL: OpenURLAction) async {
        errorMessage = ""
        cancelDeviceAuthorization()

        do {
            let flow = try await client.startDeviceAuth(providerID: provider.id)
            deviceAuth = flow
            openVerificationPage(for: flow, openURL: openURL)
            beginPolling(for: flow)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func reopenDeviceVerification(openURL: OpenURLAction) {
        guard let flow = deviceAuth else {
            return
        }
        openVerificationPage(for: flow, openURL: openURL)
    }

    func cancelDeviceAuthorization() {
        devicePollTask?.cancel()
        devicePollTask = nil
        deviceAuth = nil
    }

    func signOut() async {
        let currentSessionID = session?.sessionID
        cancelDeviceAuthorization()
        clearSessionState()

        if let currentSessionID {
            do {
                try await client.logout(sessionID: currentSessionID)
            } catch {
                errorMessage = error.localizedDescription
            }
        }

        AppConfig.clearSessionID(for: serverURL)
        await loadProviders()
    }

    func refreshPublicWaddles() async {
        guard let session else { return }
        isLoadingWaddles = true
        defer { isLoadingWaddles = false }

        do {
            let loaded = try await client.listPublicWaddles(
                sessionID: session.sessionID,
                query: searchQuery
            )
            publicWaddles = loaded
            if selectedWaddleID == nil {
                selectedWaddleID = loaded.first?.id
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func schedulePublicWaddleSearch() {
        searchTask?.cancel()
        searchTask = Task {
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            await refreshPublicWaddles()
        }
    }

    func join(_ waddle: WaddleSummary) async {
        guard let session else { return }

        do {
            try await client.joinWaddle(sessionID: session.sessionID, waddleID: waddle.id)
            joinedWaddleIDs.insert(waddle.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createWaddle(name: String, description: String?, isPublic: Bool) async {
        guard let session else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Waddle name is required."
            return
        }

        isCreatingWaddle = true
        defer { isCreatingWaddle = false }

        do {
            let created = try await client.createWaddle(
                sessionID: session.sessionID,
                name: trimmedName,
                description: description,
                isPublic: isPublic
            )
            joinedWaddleIDs.insert(created.id)
            publicWaddles.insert(created, at: 0)
            selectedWaddleID = created.id
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func isJoined(_ waddleID: String) -> Bool {
        joinedWaddleIDs.contains(waddleID)
    }

    private func beginPolling(for flow: DeviceStartResponse) {
        devicePollTask?.cancel()
        devicePollTask = Task {
            while !Task.isCancelled {
                do {
                    let result = try await client.pollDeviceAuth(deviceCode: flow.deviceCode)
                    switch result {
                    case .pending:
                        break
                    case let .complete(complete):
                        try await finalizeSignedInState(sessionID: complete.sessionID)
                        return
                    }
                } catch {
                    errorMessage = error.localizedDescription
                    cancelDeviceAuthorization()
                    return
                }

                try? await Task.sleep(nanoseconds: UInt64(flow.interval) * 1_000_000_000)
            }
        }
    }

    private func openVerificationPage(for flow: DeviceStartResponse, openURL: OpenURLAction) {
        if let url = verificationURL(for: flow) {
            openURL(url)
            return
        }

        errorMessage = "Unable to open verification URL."
    }

    private func verificationURL(for flow: DeviceStartResponse) -> URL? {
        var components = URLComponents(url: serverURL, resolvingAgainstBaseURL: false)
        components?.path = "/api/auth/device/verify"
        components?.queryItems = [URLQueryItem(name: "code", value: flow.userCode)]
        if let url = components?.url {
            return url
        }

        return normalizedVerificationURL(from: flow.verificationURIComplete)
    }

    private func normalizedVerificationURL(from raw: String) -> URL? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if let parsed = URL(string: trimmed),
           !trimmed.contains("%22"),
           !trimmed.contains("%5C%22") {
            return parsed
        }

        let decoded = trimmed.removingPercentEncoding ?? trimmed
        let unescaped = decoded
            .replacingOccurrences(of: "\\\"", with: "")
            .replacingOccurrences(of: "\"", with: "")
        return URL(string: unescaped)
    }

    private func finalizeSignedInState(sessionID: String) async throws {
        guard let loaded = try await client.session(sessionID: sessionID), !loaded.isExpired else {
            throw WaddleAPIError.server(statusCode: 401, message: "Session is not available.")
        }

        AppConfig.saveSessionID(loaded.sessionID, for: serverURL)
        session = loaded
        deviceAuth = nil
        errorMessage = ""
        await refreshPublicWaddles()
    }

    private func clearSessionState() {
        session = nil
        publicWaddles = []
        selectedWaddleID = nil
        joinedWaddleIDs.removeAll()
    }
}
