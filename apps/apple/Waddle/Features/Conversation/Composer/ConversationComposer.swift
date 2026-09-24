import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import WaddleKit

/// The Slack-style message composer: slash and mention suggestions,
/// notices, the reply or edit banner, then one card holding the
/// formatting bar, the text field, pending attachments and the action row.
struct ConversationComposer: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    let model: ComposerModel
    let conversation: ConversationID
    /// XEP-0201 thread replies go to, or nil for the main feed.
    let thread: String?
    let placeholder: String

    @FocusState private var isFocused: Bool
    /// Unicode-scalar selection in the draft; nil where the system does
    /// not report one (before iOS 18 / macOS 15).
    @State private var selection: Range<Int>?
    /// The draft `selection` was measured against: what the field itself
    /// last wrote, or the result of a formatting edit. Any other change to
    /// the draft (a completed mention or command, a sent message) moves
    /// the caret to the end instead of leaving it at a stale offset.
    @State private var selectionBase = ""
    @State private var showsFormatting = false
    @State private var showsPhotoPicker = false
    @State private var showsFileImporter = false
    @State private var photoSelection: [PhotosPickerItem] = []
    @State private var gifSearch: ComposerGifSearch?
    @State private var linkPrompt = ComposerLinkPrompt()
    @State private var commands = ComposerCommandCenter()

    var body: some View {
        let slash = slashSuggestions
        let mentions = slash == nil ? mentionSuggestions : nil
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            if let slash, !slash.isEmpty {
                ComposerSlashSuggestions(candidates: slash) { candidate in
                    complete(with: candidate)
                }
            }
            if let mentions, !mentions.candidates.isEmpty {
                ComposerMentionSuggestions(candidates: mentions.candidates) { candidate in
                    model.insertMention(candidate, replacing: mentions.query)
                }
            }
            if let error = model.errorMessage {
                ComposerErrorLine(message: error) { model.errorMessage = nil }
            }
            ComposerNoticeLine(notice: commands.notice, runningCommand: commands.running?.name) {
                commands.dismissNotice()
            }
            ComposerContextBanner(model: model)
            ComposerCard(
                model: model,
                text: fieldText,
                selection: $selection,
                showsFormatting: $showsFormatting,
                isFocused: $isFocused,
                placeholder: placeholder,
                showsMention: conversation.isRoom,
                uploader: uploader,
                actions: cardActions(slash: slash, mentions: mentions)
            )
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.top, Theme.Spacing.s)
        .padding(.bottom, Theme.Spacing.s)
        .background(.bar)
        .modifier(ComposerAttachmentPickers(
            showsPhotoPicker: $showsPhotoPicker,
            showsFileImporter: $showsFileImporter,
            photoSelection: $photoSelection,
            intake: intake
        ))
        .modifier(ComposerCommandPresentations(
            gifSearch: $gifSearch,
            linkPrompt: $linkPrompt,
            commands: commands,
            onPickGIF: sendGIF,
            onAddLink: insertLink
        ))
        .onChange(of: model.text) { oldValue, newValue in
            reportTyping(old: oldValue, new: newValue)
            moveCaretToEndIfChangedElsewhere(newValue)
        }
        .onChange(of: actions.focusRequest) { _, _ in
            isFocused = true
        }
        .task(id: commands.notice?.id) {
            await expireInfoNotice()
        }
    }

    /// The draft as the field edits it, noting what the field wrote.
    private var fieldText: Binding<String> {
        Binding(
            get: { model.text },
            set: { newValue in
                selectionBase = newValue
                model.text = newValue
            }
        )
    }

    private var uploader: AttachmentUploader {
        AttachmentUploader(session: session)
    }

    private var intake: ComposerAttachmentIntake {
        ComposerAttachmentIntake(model: model, uploader: uploader)
    }

    /// Rooms pass their JID to extension commands; 1:1 conversations none.
    private var commandRoom: BareJID? {
        conversation.isRoom ? conversation.jid : nil
    }

    private func cardActions(
        slash: [SlashCandidate]?,
        mentions: (query: MentionQuery, candidates: [MentionCandidate])?
    ) -> ComposerActions {
        ComposerActions(
            submit: submit,
            acceptSuggestion: { key in acceptSuggestion(key, slash: slash, mentions: mentions) },
            cancelContext: cancelContext,
            format: applyFormat,
            requestLink: { linkPrompt = ComposerLinkPrompt(isPresented: true) },
            insertEmoji: insertEmoji,
            startMention: startMention,
            startCommand: startCommand,
            pickPhoto: { showsPhotoPicker = true },
            pickFile: { showsFileImporter = true },
            pickGIF: { gifSearch = ComposerGifSearch(query: "") },
            paste: paste
        )
    }

    // MARK: - Suggestions

    /// Slash commands while the command word is typed, not while editing.
    private var slashSuggestions: [SlashCandidate]? {
        guard !model.isEditing, let prefix = SlashPopover.prefix(in: model.text) else { return nil }
        let candidates = SlashCandidates.filter(prefix: prefix, extensions: session.extensionCommands, inRoom: conversation.isRoom)
        return Array(candidates.prefix(8))
    }

    /// Mention completion for the `@word` at the end of the draft, in rooms
    /// only (1:1 has nobody else to address).
    private var mentionSuggestions: (query: MentionQuery, candidates: [MentionCandidate])? {
        guard conversation.isRoom, !model.isEditing, let query = MentionTokens.trailingQuery(in: model.text) else {
            return nil
        }
        let occupants = Array((session.presence.occupants[conversation.jid] ?? [:]).values)
        let candidates = MentionCandidates.matching(query.text, occupants: occupants, ownNick: session.account.nick)
        return (query, candidates)
    }

    /// Tab accepts the first suggestion. Return does too, except on a
    /// fully typed command, which Return runs.
    private func acceptSuggestion(
        _ key: ComposerSuggestionKey,
        slash: [SlashCandidate]?,
        mentions: (query: MentionQuery, candidates: [MentionCandidate])?
    ) -> Bool {
        if let slash, let first = slash.first {
            if key == .returnKey, isResolvedCommand { return false }
            complete(with: first)
            return true
        }
        if let mentions, let first = mentions.candidates.first {
            model.insertMention(first, replacing: mentions.query)
            return true
        }
        return false
    }

    private var isResolvedCommand: Bool {
        guard let trigger = SlashTrigger.parse(model.text) else { return false }
        let resolution = SlashCandidates.resolve(
            prefix: trigger.prefix,
            extensions: session.extensionCommands,
            inRoom: conversation.isRoom
        )
        return resolution != nil
    }

    private func complete(with candidate: SlashCandidate) {
        model.text = SlashCompletion.complete(text: model.text, with: candidate)
        model.errorMessage = nil
        isFocused = true
    }

    // MARK: - Sending and slash commands

    private func submit() {
        guard !model.isEditing else {
            sendDraft()
            return
        }
        let decision = SlashSubmitDecision.decide(
            text: model.text,
            extensions: session.extensionCommands,
            inRoom: conversation.isRoom
        )
        switch decision {
        case .sendAsTyped:
            sendDraft()
        case let .run(action):
            perform(action)
        case let .complete(candidate):
            complete(with: candidate)
        case .choose:
            model.errorMessage = "Choose a command from the list."
        case let .unknown(command):
            model.errorMessage = "No command /\(command). Remove the / to send as text."
        }
    }

    private func perform(_ action: SlashAction) {
        model.errorMessage = nil
        switch action {
        case let .send(body):
            model.text = body
            sendDraft()
        case let .searchGIFs(query):
            model.clearText()
            gifSearch = ComposerGifSearch(query: query)
        case let .setAvailability(availability):
            model.clearText()
            let commands = self.commands
            let session = self.session
            Task { await commands.setAvailability(availability, session: session) }
        case let .runExtension(command, invocation):
            model.clearText()
            let commands = self.commands
            let session = self.session
            let room = commandRoom
            Task { await commands.run(command, invocation: invocation, room: room, session: session) }
        case .incomplete:
            // `SlashSubmitDecision` turns an incomplete command into a
            // completion before it gets here.
            break
        }
    }

    private func sendDraft() {
        guard let submission = model.takeSubmission(thread: thread) else { return }
        dispatch(submission)
    }

    /// A picked GIF is sent as a message whose body is its URL.
    private func sendGIF(_ item: GifSearchItem) {
        gifSearch = nil
        dispatch(model.takeLinkSubmission(item.originalURL, thread: thread))
    }

    private func dispatch(_ submission: ComposerSubmission) {
        let model = self.model
        let session = self.session
        let conversation = self.conversation
        Task { await model.perform(submission, session: session, in: conversation) }
    }

    /// Escape leaves edit mode, else drops the reply.
    private func cancelContext() -> Bool {
        if model.isEditing {
            model.cancelEdit()
            return true
        }
        if model.reply != nil {
            model.cancelReply()
            return true
        }
        return false
    }

    // MARK: - Editing the draft

    private func applyFormat(_ format: ComposerFormat) {
        apply(ComposerFormatting.apply(format, to: model.text, selection: selection))
    }

    private func insertEmoji(_ emoji: String) {
        apply(ComposerInsertion.replacingSelection(with: emoji, in: model.text, selection: selection))
    }

    private func insertLink(_ input: String) {
        guard let url = ComposerInsertion.linkURL(from: input) else {
            model.errorMessage = "That doesn't look like a web address."
            return
        }
        apply(ComposerInsertion.insertingLink(url, into: model.text, selection: selection))
    }

    /// Puts `@` at the end so the mention list opens.
    private func startMention() {
        model.text = ComposerInsertion.appendingToken("@", to: model.text)
        isFocused = true
    }

    /// Opens the command list: `/` alone, or `/ ` in front of the draft,
    /// which completing a command keeps as its argument.
    private func startCommand() {
        if SlashTrigger.parse(model.text) == nil {
            model.text = model.text.isEmpty ? "/" : "/ " + model.text
        }
        isFocused = true
    }

    private func apply(_ edit: ComposerTextEdit) {
        selectionBase = edit.text
        model.text = edit.text
        if ComposerSelectionSupport.isAvailable {
            selection = edit.selection
        }
        isFocused = true
    }

    private func moveCaretToEndIfChangedElsewhere(_ text: String) {
        guard text != selectionBase else { return }
        selectionBase = text
        guard selection != nil else { return }
        let end = text.unicodeScalars.count
        selection = end..<end
    }

    // MARK: - Paste

    private func paste() {
        switch ComposerPasteboard.read() {
        case let .text(string):
            guard let string, !string.isEmpty else { return }
            apply(ComposerInsertion.replacingSelection(with: string, in: model.text, selection: selection))
        case let .attachments(contents, text):
            guard !model.isEditing else {
                model.errorMessage = "Attachments can't be added while editing a message."
                return
            }
            intake.attachPasted(contents)
            if let text {
                apply(ComposerInsertion.replacingSelection(with: text, in: model.text, selection: selection))
            }
        }
    }

    // MARK: - Typing and notices

    /// XEP-0085: composing on edits, active when the draft is cleared.
    /// Editing an old message is not composing a new one.
    private func reportTyping(old: String, new: String) {
        guard !model.isEditing, old != new else { return }
        if new.isEmpty {
            session.stopTyping(in: conversation, notify: true)
        } else {
            session.userTyped(in: conversation)
        }
    }

    /// Info notices fade after a few seconds; warnings and errors stay
    /// until dismissed.
    private func expireInfoNotice() async {
        guard let notice = commands.notice, notice.severity == .info else { return }
        try? await Task.sleep(nanoseconds: 5_000_000_000)
        guard !Task.isCancelled, commands.notice?.id == notice.id else { return }
        commands.dismissNotice()
    }
}
