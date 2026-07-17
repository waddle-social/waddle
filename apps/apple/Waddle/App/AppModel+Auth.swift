import Foundation
import SwiftUI

// MARK: - Authentication & Session Lifecycle

extension AppModel {
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
        await clearSessionState()
        await bootstrap()
    }

    func bootstrap() async {
        errorMessage = ""
        await loadProviders()

        guard let storedSessionID = AppConfig.storedSessionID(for: serverURL) else {
            updateChatSurfaceState()
            return
        }

        do {
            guard let loaded = try await client.session(sessionID: storedSessionID), !loaded.isExpired else {
                AppConfig.clearSessionID(for: serverURL)
                updateChatSurfaceState()
                return
            }
            await applyAuthenticatedSession(loaded, persistSession: false)
        } catch {
            errorMessage = error.localizedDescription
            updateChatSurfaceState()
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
        await clearSessionState()

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

    private func beginPolling(for flow: DeviceStartResponse) {
        devicePollTask?.cancel()
        devicePollTask = Task {
            while !Task.isCancelled {
                do {
                    let result = try await client.pollDeviceAuth(deviceCode: flow.deviceCode)
                    errorMessage = ""
                    switch result {
                    case .pending:
                        break
                    case .complete(let complete):
                        try await finalizeSignedInState(sessionID: complete.sessionID)
                        return
                    }
                } catch {
                    if isTransientDeviceAuthPollError(error) {
                        errorMessage = "Connection interrupted. Retrying sign-in…"
                        try? await Task.sleep(nanoseconds: UInt64(flow.interval) * 1_000_000_000)
                        continue
                    }
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
        if let raw = flow.verificationURIComplete,
           let url = normalizedVerificationURL(from: raw) {
            return url
        }

        if let raw = flow.verificationURI,
           let base = normalizedVerificationURL(from: raw),
           var components = URLComponents(url: base, resolvingAgainstBaseURL: false) {
            components.queryItems = [URLQueryItem(name: "code", value: flow.userCode)]
            if let url = components.url {
                return url
            }
        }

        var components = URLComponents(url: serverURL, resolvingAgainstBaseURL: false)
        components?.path = "/api/auth/device/verify"
        components?.queryItems = [URLQueryItem(name: "code", value: flow.userCode)]
        return components?.url
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

    private func isTransientDeviceAuthPollError(_ error: Error) -> Bool {
        let nsError = error as NSError
        guard nsError.domain == NSURLErrorDomain else {
            return false
        }
        let code = URLError.Code(rawValue: nsError.code)

        switch code {
        case .networkConnectionLost, .timedOut:
            return true
        default:
            return false
        }
    }

    private func finalizeSignedInState(sessionID: String) async throws {
        guard let loaded = try await client.session(sessionID: sessionID), !loaded.isExpired else {
            throw WaddleAPIError.server(statusCode: 401, message: "Session is not available.")
        }

        await applyAuthenticatedSession(loaded, persistSession: true)
    }

    private func applyAuthenticatedSession(_ loaded: WaddleSession, persistSession: Bool) async {
        if persistSession {
            AppConfig.saveSessionID(loaded.sessionID, for: serverURL)
        }

        openXMPPAdmissions(
            sessionIdentity: XMPPLoginSessionIdentity(value: loaded.sessionID)
        )
        session = loaded
        deviceAuth = nil
        errorMessage = ""
        updateChatSurfaceState()

        await connectXMPP(using: loaded)
    }

    private func clearSessionState() async {
        reconnectTask?.cancel()
        reconnectTask = nil
        await closeXMPPAdmissions()
        failPendingRoomJoins(with: XMPPServiceError.disconnected)

        session = nil
        spaceName = nil
        spaces = []
        selectedSpaceID = nil
        channels = []
        selectedChannelID = nil
        members = []
        joinedRoomJIDs.removeAll()
        messagesByRoomJID.removeAll()
        presenceByRoomJID.removeAll()
        roomHistoryBeforeCursorByRoomJID.removeAll()
        chatStore.clearComposer()
        chatStore.replaceRooms([])
        chatStore.replaceMembers([])
        chatStore.replaceMessages([])
        chatStore.setBannerState(.hidden)
        chatStore.setSurfaceState(.empty(title: "Sign in", message: "Sign in to load the server space and connect to live rooms."))
        structureLoadGeneration += 1
        structureLoadSurfaceError = nil
        isLoadingStructure = false
    }
}
