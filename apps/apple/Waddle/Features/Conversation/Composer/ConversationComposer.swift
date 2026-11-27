import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import WaddleKit

/// The Slack-style message composer: suggestions,
/// notices, the reply or edit banner, then one card holding the
/// formatting bar, the text field, pending attachments and the action row.
struct ConversationComposer: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @Environment(AppState.self) private var app
    let model: ComposerModel
    let conversation: ConversationID
    /// XEP-0201 thread replies go to, or nil for the main feed. A
    /// sub-thread's parent is the session's to work out.
    let thread: String?
    let placeholder: String

    /// Whether the text field is being edited; the field keeps it in step
    /// with the system's focus.
    @State private var isFocused = false
    @State private var selection: ComposerScalarRange?
    /// The draft `selection` was measured against: what the field itself
    /// last wrote, or the result of a composer edit. An external change
    /// to the draft, such as a send, moves the caret to the end.
    @State private var selectionBase = ""
    @State private var fieldEdits = ComposerFieldEdits()
    @State private var draftRevision = 0
    @State private var showsFormatting = false
    @State private var formatRevision = 0
    @State private var showsPhotoPicker = false
    @State private var showsFileImporter = false
    @State private var photoSelection: [PhotosPickerItem] = []
    @State private var gifSearch: ComposerGifSearch?
    @State private var showsStickers = false
    @State private var linkPrompt = ComposerLinkPrompt()
    @State private var commands = ComposerCommandCenter()
    @State private var suggestionDirectory = ComposerSuggestionDirectory()
    @State private var suggestionNavigation: ComposerSuggestionSelection.Navigation?
    /// Counts sends, to play the send haptic.
    @State private var sendCount = 0

    var body: some View {
        let presentation = suggestionPresentation
        let composerCard = card(showsSuggestions: presentation.list.selectedIndex != nil)
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            // Suggestions and notices open away from the screen edge the
            // composer sits on: above it at the bottom, below it at the
            // top in the social order.
            if isTopPinned {
                ComposerContextBanner(model: model)
                composerCard
                suggestionsAndNotices(presentation.list)
            } else {
                suggestionsAndNotices(presentation.list)
                ComposerContextBanner(model: model)
                composerCard
            }
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.top, Theme.Spacing.s)
        .padding(.bottom, Theme.Spacing.s)
        .background(Theme.Colors.bg)
        .modifier(ComposerAttachmentPickers(
            showsPhotoPicker: $showsPhotoPicker,
            showsFileImporter: $showsFileImporter,
            photoSelection: $photoSelection,
            intake: intake
        ))
        .modifier(ComposerCommandPresentations(
            gifSearch: $gifSearch,
            showsStickers: $showsStickers,
            linkPrompt: $linkPrompt,
            commands: commands,
            onPickGIF: sendGIF,
            onPickSticker: sendSticker,
            onAddLink: insertLink
        ))
        .onChange(of: model.text) { oldValue, newValue in
            reportTyping(old: oldValue, new: newValue)
            moveCaretToEndIfChangedElsewhere(newValue)
        }
        .onChange(of: actions.focusRequest) { _, _ in
            isFocused = true
        }
        .sensoryFeedback(.impact(flexibility: .soft, intensity: 0.7), trigger: sendCount)
        .task(id: commands.notice?.id) {
            await expireInfoNotice()
        }
        // Keep the cursor identity across result changes. The rows are derived above.
        .onChange(of: presentation.list.navigation) { _, navigation in
            if navigation != suggestionNavigation { suggestionNavigation = navigation }
        }
        .task(id: presentation.directoryQuery) {
            await suggestionDirectory.search(presentation.directoryQuery, using: session)
        }
    }

    private var isTopPinned: Bool {
        app.preferences.messageOrder.isNewestFirst
    }

    private func card(showsSuggestions: Bool) -> some View {
        let actions = cardActions
        var suppression: ComposerBubbleVisibility.Suppression = []
        if showsSuggestions { suppression.insert(.suggestions) }
        if linkPrompt.isPresented { suppression.insert(.linkPrompt) }
        if showsFormatting { suppression.insert(.formattingBar) }
        return ComposerCard(
            model: model,
            draft: fieldDraft,
            edits: fieldEdits,
            selection: $selection,
            showsFormatting: $showsFormatting,
            isFocused: $isFocused,
            placeholder: placeholder,
            showsMention: conversation.isRoom,
            selectionFormats: ComposerSelectionFormatCommands(
                suppression: suppression, formatRevision: formatRevision, roomLinks: model.references.roomLinks
            ) { action in
                action.perform(onFormat: actions.keys.format, onLink: actions.requestLink)
            },
            uploader: uploader,
            actions: actions
        )
    }

    @ViewBuilder
    private func suggestionsAndNotices(_ list: ComposerSuggestionSelection) -> some View {
        if let selected = list.selectedIndex, let trigger = list.trigger {
            ComposerSuggestionList(items: list.items, selectedIndex: selected) { item in
                if let pick = suggestionPresentation.pick(item, from: trigger, in: model.text) { apply(pick) }
            }
        }
        if let error = model.errorMessage {
            ComposerErrorLine(message: error) { model.errorMessage = nil }
        }
        ComposerNoticeLine(notice: commands.notice, runningCommand: commands.running?.name) {
            commands.dismissNotice()
        }
    }

    private func fieldDidChange(_ replacement: ComposerTextReplacement) {
        selectionBase = replacement.text
        model.replaceText(replacement)
    }

    private var uploader: AttachmentUploader {
        AttachmentUploader(session: session, protection: .encrypted)
    }

    private var intake: ComposerAttachmentIntake {
        ComposerAttachmentIntake(model: model, uploader: uploader)
    }

    /// Extensions act in channels. A group DM is a conversation between
    /// people like a 1:1 one, and the server refuses bots there.
    private var isChannel: Bool {
        conversation.isRoom && session.directory.channel(for: conversation.jid)?.isGroupDM != true
    }

    /// Channels pass their JID to extension commands; DMs none.
    private var commandRoom: BareJID? {
        isChannel ? conversation.jid : nil
    }

    private var cardActions: ComposerActions {
        ComposerActions(
            submit: submit,
            changeText: fieldDidChange,
            keys: ComposerKeyInput(
                suggestion: handleSuggestionKey, edit: apply, format: applyFormat, submit: submit, cancel: cancelContext
            ),
            requestLink: { linkPrompt = ComposerLinkPrompt(isPresented: true) },
            insertEmoji: insertEmoji,
            startMention: startMention,
            startCommand: startCommand,
            pickPhoto: { showsPhotoPicker = true },
            pickFile: { showsFileImporter = true },
            pickGIF: { gifSearch = ComposerGifSearch(query: "") },
            pickSticker: { showsStickers = true },
            paste: paste
        )
    }

    // MARK: - Suggestions

    private var currentSelection: ComposerScalarRange? {
        model.text == selectionBase ? selection : nil
    }

    private var suggestionPresentation: ComposerSuggestionsModel {
        ComposerSuggestionsModel.present(
            draft: .init(
                text: model.text, selection: currentSelection,
                isEditing: model.isEditing, roomLinks: model.references.roomLinks
            ),
            place: .init(conversation: conversation, inChannel: isChannel, account: session.account),
            occupants: Array((session.presence.occupants[conversation.jid] ?? [:]).values),
            extensions: session.extensionCommands, directory: suggestionDirectory.results,
            navigation: suggestionNavigation, rooms: session.directory.channels
        )
    }

    private func handleSuggestionKey(_ key: ComposerHardwareKey) -> Bool {
        switch suggestionPresentation.handle(key, in: model.text) {
        case .pass: return false
        case let .navigate(navigation): suggestionNavigation = navigation
        case let .pick(pick): apply(pick)
        }
        return true
    }

    private func apply(_ pick: ComposerSuggestionsModel.Pick) {
        record(pick.edit, replacement: ComposerTextReplacement(pick.edit.text, replacing: pick.replacementRange))
        model.accept(pick)
    }

    // MARK: - Sending and slash commands

    private func submit() {
        // An open list takes Send before anything else, in an edit too, as it takes Return.
        if let pick = suggestionPresentation.pickForSubmit(in: model.text) { return apply(pick) }
        guard !model.isEditing else {
            sendDraft()
            return
        }
        let decision = SlashSubmitDecision.decide(
            text: model.text,
            extensions: session.extensionCommands,
            inRoom: isChannel
        )
        switch decision {
        case .sendAsTyped:
            sendDraft()
        case let .run(action):
            perform(action)
        case let .complete(candidate):
            applySlashText(SlashCompletion.complete(text: model.text, with: candidate))
            model.errorMessage = nil
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
            applySlashText(body)
            sendDraft()
        case let .searchGIFs(query):
            model.clearText()
            gifSearch = ComposerGifSearch(query: query)
        case let .setPresenceMode(mode):
            model.clearText()
            let commands = self.commands
            let session = self.session
            Task { await commands.setPresenceMode(mode, session: session) }
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
        draftRevision += 1
        dispatch(submission)
    }

    /// A picked GIF is sent as a message whose body is its URL.
    private func sendGIF(_ item: GifSearchItem) {
        gifSearch = nil
        dispatch(model.takeLinkSubmission(item.originalURL, thread: thread))
    }

    private func sendSticker(_ sticker: Sticker, from pack: StickerPack) {
        showsStickers = false
        dispatch(model.takeStickerSubmission(sticker, from: pack, thread: thread))
    }

    private func dispatch(_ submission: ComposerSubmission) {
        sendCount += 1
        let model = self.model
        let session = self.session
        let conversation = self.conversation
        Task { await model.perform(submission, session: session, in: conversation) }
    }

    /// Escape with the suggestion list closed leaves edit mode, else drops the reply.
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
        apply(ComposerFormatting.apply(
            format, to: model.text, selection: currentSelection, roomLinks: model.references.roomLinks
        ))
        formatRevision += 1
    }

    private func insertEmoji(_ emoji: String) {
        replaceSelection(with: emoji)
    }

    private func insertLink(_ input: String) {
        guard let url = ComposerInsertion.linkURL(from: input) else {
            model.errorMessage = "That doesn't look like a web address."
            return
        }
        let selection = currentSelection
        let point = selection?.upperBound ?? model.text.unicodeScalars.count
        apply(ComposerInsertion.insertingLink(url, into: model.text, selection: selection), replacing: point..<point)
    }

    /// Puts `@` at the end so the mention list opens.
    private func startMention() {
        let point = model.text.unicodeScalars.count
        let text = ComposerInsertion.appendingToken("@", to: model.text)
        let end = text.unicodeScalars.count
        apply(ComposerTextEdit(text: text, selection: end..<end), replacing: point..<point)
    }

    private func startCommand() {
        suggestionNavigation = nil
        apply(SlashCompletion.start(in: model.text), replacing: 0..<0)
    }

    private func replaceSelection(with text: String) {
        apply(ComposerTextReplacement.replacingSelection(with: text, in: model.text, selection: currentSelection))
    }

    private func apply(_ edit: ComposerTextEdit, replacing range: ComposerScalarRange) {
        apply((edit, ComposerTextReplacement(edit.text, replacing: range)))
    }

    private func apply(_ change: (edit: ComposerTextEdit, replacement: ComposerTextReplacement)) {
        record(change.edit, replacement: change.replacement)
        model.replaceText(change.replacement)
    }

    private func apply(_ edit: ComposerTextEdit) {
        apply((edit, edit.replacement))
    }

    private func applySlashText(_ text: String) {
        let end = text.unicodeScalars.count
        apply(ComposerTextEdit(text: text, selection: end..<end))
    }

    private var fieldDraft: ComposerFieldEdits.Draft {
        .init(model: ObjectIdentifier(model), editing: model.editing?.presentationID, revision: draftRevision)
    }

    private func record(_ edit: ComposerTextEdit, replacement: ComposerTextReplacement) {
        fieldEdits.append(.init(original: model.text, replacement: replacement, selection: edit.selection), in: fieldDraft)
        selectionBase = edit.text
        selection = edit.selection
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
            replaceSelection(with: string)
        case let .attachments(contents, text):
            guard !model.isEditing else {
                model.errorMessage = "Attachments can't be added while editing a message."
                return
            }
            intake.attachPasted(contents)
            if let text {
                replaceSelection(with: text)
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
