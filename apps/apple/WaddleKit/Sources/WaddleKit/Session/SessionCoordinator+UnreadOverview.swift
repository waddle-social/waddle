import Foundation

extension SessionCoordinator {
    /// How many sections are fetched at once.
    static var overviewConcurrency: Int { 4 }

    /// Rooms the Activity overview would list right now, without fetching.
    public var unreadOverviewRoomCount: Int {
        overviewCandidates().count
    }

    /// Rebuilds the Activity overview from the current unread state: every
    /// room with an unread room or thread row, newest first, with its
    /// unread messages fetched over XEP-0313 (the room archive, and the
    /// Waddle thread filter for each thread). Sections whose unread state
    /// is unchanged since the last refresh are not refetched.
    public func refreshUnreadOverview() async {
        let refresh = unreadOverview.begin()
        let epoch = connectionEpoch
        let candidates = overviewCandidates()
        unreadOverview.publish(overviewGroups(candidates, failed: []), refresh: refresh)

        let sections = candidates.flatMap(Self.sections(of:))
        let missing = sections.filter { unreadOverview.cached($0) == nil }
        guard connection == .online, !missing.isEmpty else {
            unreadOverview.finish(refresh: refresh, keeping: Set(sections), didFail: false)
            return
        }

        let results = await fetchOverviewSections(missing)
        guard epoch == connectionEpoch, unreadOverview.isCurrent(refresh) else { return }
        var failed = Set<UnreadOverviewSection>()
        for section in missing {
            guard case let .success(page)? = results[section] else {
                failed.insert(section)
                continue
            }
            unreadOverview.remember(overviewMessages(in: page, for: section), for: section, refresh: refresh)
        }
        unreadOverview.publish(overviewGroups(candidates, failed: failed), refresh: refresh)
        unreadOverview.finish(refresh: refresh, keeping: Set(sections), didFail: failed.count == missing.count)
    }

    /// Marks every room and thread in the overview read.
    public func markOverviewRead() async {
        for candidate in overviewCandidates() {
            if candidate.unread > 0 {
                await markDisplayed(.room(candidate.room))
            }
            for thread in candidate.threads {
                await markThreadRead(thread.key)
            }
        }
    }

    func overviewCandidates() -> [UnreadOverviewCandidate] {
        UnreadOverview.candidates(
            counts: unread.counts,
            threadCounts: unread.threadCounts,
            entry: { [inbox] room, thread in inbox.entry(for: room, threadID: thread) },
            isRoom: { [directory] in directory.isRoom($0) },
            threadTitle: { _, row in UnreadOverview.threadTitle(row) }
        )
    }

    private static func sections(of candidate: UnreadOverviewCandidate) -> [UnreadOverviewSection] {
        var sections: [UnreadOverviewSection] = []
        if candidate.unread > 0 {
            sections.append(UnreadOverviewSection(
                room: candidate.room,
                threadID: nil,
                unread: candidate.unread,
                lastStanzaID: candidate.lastStanzaID
            ))
        }
        for thread in candidate.threads {
            sections.append(UnreadOverviewSection(
                room: candidate.room,
                threadID: thread.key.threadID,
                unread: thread.unread,
                lastStanzaID: thread.lastStanzaID
            ))
        }
        return sections
    }

    private func overviewGroups(_ candidates: [UnreadOverviewCandidate], failed: Set<UnreadOverviewSection>) -> [UnreadOverviewGroup] {
        candidates.map { candidate in
            let sections = Self.sections(of: candidate)
            let feed = sections.first { $0.threadID == nil }
            let threads = zip(candidate.threads, sections.filter { $0.threadID != nil }).map { thread, section in
                UnreadOverviewThread(
                    key: thread.key,
                    title: thread.title,
                    unread: thread.unread,
                    lastUpdated: thread.lastUpdated,
                    messages: unreadOverview.cached(section) ?? []
                )
            }
            return UnreadOverviewGroup(
                room: candidate.room,
                title: directory.title(for: .room(candidate.room)),
                unread: candidate.unread,
                mentionsMe: unread.mentions.contains(.room(candidate.room)),
                lastUpdated: candidate.lastUpdated,
                messages: feed.flatMap { unreadOverview.cached($0) } ?? [],
                threads: threads,
                isIncomplete: !failed.isDisjoint(with: sections)
            )
        }
    }

    private func overviewMessages(in page: ArchivePage, for section: UnreadOverviewSection) -> [TimelineItem] {
        if let threadID = section.threadID {
            return UnreadOverview.threadMessages(
                from: page,
                thread: ThreadKey(room: section.room, threadID: threadID),
                unread: section.unread,
                account: account
            )
        }
        return UnreadOverview.roomMessages(
            from: page,
            room: section.room,
            unread: section.unread,
            readCursor: readCursors.cursor(.room(section.room)),
            account: account
        )
    }

    /// Fetches sections with at most `overviewConcurrency` queries in flight.
    private func fetchOverviewSections(_ sections: [UnreadOverviewSection]) async -> [UnreadOverviewSection: Result<ArchivePage, any Error>] {
        let port = self.port
        return await withTaskGroup(of: (UnreadOverviewSection, Result<ArchivePage, any Error>).self) { group in
            var results: [UnreadOverviewSection: Result<ArchivePage, any Error>] = [:]
            var pending = sections[...]
            var inFlight = 0
            while inFlight < Self.overviewConcurrency, let section = pending.popFirst() {
                group.addTask { await (section, Self.fetch(section, from: port)) }
                inFlight += 1
            }
            while let (section, result) = await group.next() {
                results[section] = result
                if let next = pending.popFirst() {
                    group.addTask { await (next, Self.fetch(next, from: port)) }
                }
            }
            return results
        }
    }

    private nonisolated static func fetch(_ section: UnreadOverviewSection, from port: any XmppPort) async -> Result<ArchivePage, any Error> {
        let max = UnreadOverview.fetchSize(for: section.unread)
        do {
            if let threadID = section.threadID {
                return .success(try await port.fetchThreadHistory(in: section.room, threadID: threadID, before: nil, max: max))
            }
            return .success(try await port.fetchHistory(of: .room(section.room), before: nil, max: max))
        } catch {
            return .failure(error)
        }
    }
}
