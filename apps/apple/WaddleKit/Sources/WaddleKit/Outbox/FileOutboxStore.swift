import Foundation

/// Saves the outbox as one small JSON file, rewritten atomically on every
/// change. On Apple platforms the file is readable once the device has been
/// unlocked after boot (a locked-device background wake must still be able
/// to write it) and is kept out of backups.
@MainActor
public struct FileOutboxStore: OutboxStore {
    public let url: URL

    public init(url: URL) {
        self.url = url
    }

    public func load() throws -> [PersistedOutbound] {
        guard FileManager.default.fileExists(atPath: url.path) else { return [] }
        let data = try Data(contentsOf: url)
        switch OutboxFile.decode(data) {
        case let .success(entries):
            return entries
        case let .failure(error):
            OutboxLog.error("Discarding unreadable outbox: \(error)")
            remove()
            return []
        }
    }

    public func save(_ entries: [PersistedOutbound]) throws {
        guard !entries.isEmpty else {
            remove()
            return
        }
        let data = try OutboxFile.encode(entries)
        let directory = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try data.write(to: url, options: Self.writeOptions)
        // An atomic write replaces the file, dropping its resource values.
        try Self.excludeFromBackup(url)
    }

    public func remove() {
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        do {
            try FileManager.default.removeItem(at: url)
        } catch {
            OutboxLog.error("Could not remove outbox: \(error)")
        }
    }

    #if canImport(Darwin)
    private static let writeOptions: Data.WritingOptions = [.atomic, .completeFileProtectionUntilFirstUserAuthentication]

    private static func excludeFromBackup(_ url: URL) throws {
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var target = url
        try target.setResourceValues(values)
    }
    #else
    private static let writeOptions: Data.WritingOptions = [.atomic]

    private static func excludeFromBackup(_ url: URL) throws {}
    #endif
}
