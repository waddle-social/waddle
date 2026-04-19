import Foundation

struct XMPPElement: Sendable, Equatable {
    let name: String
    let attributes: [String: String]
    let children: [XMPPElement]
    let text: String

    var localName: String {
        name.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false).last.map(String.init) ?? name
    }

    func attribute(_ key: String) -> String? {
        attributes[key]
    }

    func firstChild(named localName: String) -> XMPPElement? {
        children.first { $0.localName == localName }
    }

    func children(named localName: String) -> [XMPPElement] {
        children.filter { $0.localName == localName }
    }
}

private final class XMPPFragmentParser: NSObject, XMLParserDelegate {
    private struct StackItem {
        var name: String
        var attributes: [String: String]
        var children: [XMPPElement] = []
        var text = ""
    }

    private var stack: [StackItem] = []
    private var root: XMPPElement?

    func parse(_ xml: String) -> XMPPElement? {
        let parser = XMLParser(data: Data(xml.utf8))
        parser.delegate = self
        parser.shouldProcessNamespaces = false
        parser.shouldResolveExternalEntities = false
        stack.removeAll()
        root = nil
        guard parser.parse() else {
            return nil
        }
        return root
    }

    func parser(
        _ parser: XMLParser,
        didStartElement elementName: String,
        namespaceURI: String?,
        qualifiedName qName: String?,
        attributes attributeDict: [String : String] = [:]
    ) {
        stack.append(StackItem(name: qName ?? elementName, attributes: attributeDict))
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) {
        guard !stack.isEmpty else { return }
        stack[stack.count - 1].text += string
    }

    func parser(
        _ parser: XMLParser,
        didEndElement elementName: String,
        namespaceURI: String?,
        qualifiedName qName: String?
    ) {
        guard let item = stack.popLast() else { return }
        let element = XMPPElement(
            name: item.name,
            attributes: item.attributes,
            children: item.children,
            text: item.text.trimmingCharacters(in: .whitespacesAndNewlines)
        )

        if var parent = stack.popLast() {
            parent.children.append(element)
            stack.append(parent)
        } else {
            root = element
        }
    }
}

enum XMPPXML {
    static func openStream(to domain: String) -> String {
        "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' to='\(escape(domain))' version='1.0'/>"
    }

    static func authenticationRequest(jid: String, bearerToken: String) -> String {
        let payload = "n,a=\(jid)\u{1}auth=Bearer \(bearerToken)\u{1}\u{1}"
        return "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='OAUTHBEARER'>\(Data(payload.utf8).base64EncodedString())</auth>"
    }

    static func bind(resource: String, id: String) -> String {
        "<iq type='set' id='\(escape(id))'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>\(escape(resource))</resource></bind></iq>"
    }

    static func requestSession(id: String) -> String {
        "<iq type='set' id='\(escape(id))'><session xmlns='urn:ietf:params:xml:ns:xmpp-session'/></iq>"
    }

    static func presence(to jid: String? = nil, show: String? = nil, status: String? = nil) -> String {
        var attributes = ""
        if let jid {
            attributes += " to='\(escape(jid))'"
        }

        var payload = "<presence\(attributes)>"
        if let show, !show.isEmpty {
            payload += "<show>\(escape(show))</show>"
        }
        if let status, !status.isEmpty {
            payload += "<status>\(escape(status))</status>"
        }
        payload += "</presence>"
        return payload
    }

    static func joinRoom(roomJID: String, nick: String) -> String {
        "<presence to='\(escape(roomJID))/\(escape(nick))'><x xmlns='http://jabber.org/protocol/muc'/></presence>"
    }

    static func chatStateMessage(to roomJID: String, state: String) -> String {
        "<message to='\(escape(roomJID))' type='groupchat'><\(escape(state)) xmlns='http://jabber.org/protocol/chatstates'/></message>"
    }

    static func displayedMarker(to roomJID: String, messageID: String) -> String {
        "<message to='\(escape(roomJID))' type='groupchat'><displayed xmlns='urn:xmpp:chat-markers:0' id='\(escape(messageID))'/></message>"
    }

    static func groupchatMessage(to roomJID: String, body: String, thread: String? = nil) -> String {
        var payload = "<message to='\(escape(roomJID))' type='groupchat'>"
        payload += "<body>\(escape(body))</body>"
        if let thread, !thread.isEmpty {
            payload += "<thread>\(escape(thread))</thread>"
        }
        payload += "</message>"
        return payload
    }

    static func groupchatReplyMessage(
        to roomJID: String,
        body: String,
        replyToID: String,
        replyToSender: String?,
        replyToBody: String?,
        thread: String? = nil
    ) -> String {
        let fallbackPrefix = buildReplyFallbackPrefix(replyToBody)
        let fullBody = fallbackPrefix + body
        var payload = "<message to='\(escape(roomJID))' type='groupchat'>"
        payload += "<body>\(escape(fullBody))</body>"
        var replyAttrs = "id='\(escape(replyToID))'"
        if let replyToSender, !replyToSender.isEmpty {
            replyAttrs += " to='\(escape(replyToSender))'"
        }
        payload += "<reply xmlns='urn:xmpp:reply:0' \(replyAttrs)/>"
        if !fallbackPrefix.isEmpty {
            payload += "<fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>"
            payload += "<body start='0' end='\(fallbackPrefix.utf16.count)'/>"
            payload += "</fallback>"
        }
        if let thread, !thread.isEmpty {
            payload += "<thread>\(escape(thread))</thread>"
        }
        payload += "</message>"
        return payload
    }

    static func buildReplyFallbackPrefix(_ parentBody: String?) -> String {
        guard let parentBody, !parentBody.isEmpty else { return "" }
        let lines = parentBody.split(separator: "\n", omittingEmptySubsequences: false)
        let quoted = lines.map { "> \($0)" }.joined(separator: "\n")
        return "\(quoted)\n\n"
    }

    static func mamRoomHistoryQuery(id: String, to roomJID: String, max: Int, before: String? = "") -> String {
        var query = "<query xmlns='urn:xmpp:mam:2' queryid='\(escape(id))'>"
        query += "<x xmlns='jabber:x:data' type='submit'>"
        query += "<field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>"
        query += "</x>"
        query += "<set xmlns='http://jabber.org/protocol/rsm'>"
        query += "<max>\(max)</max>"
        if let before {
            if before.isEmpty {
                query += "<before/>"
            } else {
                query += "<before>\(escape(before))</before>"
            }
        }
        query += "</set>"
        query += "</query>"
        return "<iq type='set' id='\(escape(id))' to='\(escape(roomJID))'>\(query)</iq>"
    }

    static func adHocCommandExecute(id: String, to: String, node: String) -> String {
        "<iq type='set' id='\(escape(id))' to='\(escape(to))'>" +
        "<command xmlns='http://jabber.org/protocol/commands' node='\(escape(node))' action='execute'/>" +
        "</iq>"
    }

    static func adHocCommandComplete(
        id: String,
        to: String,
        node: String,
        sessionID: String,
        fields: [(name: String, value: String, type: String)]
    ) -> String {
        var form = "<x xmlns='jabber:x:data' type='submit'>"
        for field in fields {
            form += "<field var='\(escape(field.name))' type='\(escape(field.type))'>"
            form += "<value>\(escape(field.value))</value>"
            form += "</field>"
        }
        form += "</x>"
        return "<iq type='set' id='\(escape(id))' to='\(escape(to))'>" +
            "<command xmlns='http://jabber.org/protocol/commands' node='\(escape(node))' action='complete' sessionid='\(escape(sessionID))'>" +
            form +
            "</command>" +
            "</iq>"
    }

    static func parseAdHocCommandResponse(from element: XMPPElement) -> (sessionID: String?, status: String?, fields: [String: String]) {
        guard let command = element.firstChild(named: "command") else {
            return (nil, nil, [:])
        }
        let sessionID = command.attribute("sessionid")
        let status = command.attribute("status")
        var fields: [String: String] = [:]
        if let form = command.firstChild(named: "x") {
            for field in form.children(named: "field") {
                if let varName = field.attribute("var"),
                   let value = field.firstChild(named: "value")?.text, !value.isEmpty {
                    fields[varName] = value
                }
            }
        }
        return (sessionID, status, fields)
    }

    static func discoItems(id: String, to: String, node: String? = nil) -> String {
        var query = "<query xmlns='http://jabber.org/protocol/disco#items'"
        if let node, !node.isEmpty {
            query += " node='\(escape(node))'"
        }
        query += "/>"
        return "<iq type='get' id='\(escape(id))' to='\(escape(to))'>\(query)</iq>"
    }

    static func discoInfo(id: String, to: String, node: String? = nil) -> String {
        var query = "<query xmlns='http://jabber.org/protocol/disco#info'"
        if let node, !node.isEmpty {
            query += " node='\(escape(node))'"
        }
        query += "/>"
        return "<iq type='get' id='\(escape(id))' to='\(escape(to))'>\(query)</iq>"
    }

    static func escape(_ value: String) -> String {
        value
            .replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
            .replacingOccurrences(of: "\"", with: "&quot;")
            .replacingOccurrences(of: "'", with: "&apos;")
    }

    static func splitDocuments(from buffer: inout String) -> [String] {
        var documents: [String] = []
        trimLeadingWhitespace(&buffer)

        while let document = consumeDocument(from: &buffer) {
            documents.append(document)
            trimLeadingWhitespace(&buffer)
        }

        return documents
    }

    static func parseDocument(_ xml: String) -> XMPPElement? {
        XMPPFragmentParser().parse(xml)
    }

    static func parseStreamFeatures(from element: XMPPElement) -> XMPPStreamFeatures? {
        guard element.localName == "features" else {
            return nil
        }

        var features = XMPPStreamFeatures()
        for child in element.children {
            switch child.localName {
            case "mechanisms":
                for mechanism in child.children(named: "mechanism") {
                    let value = mechanism.text.trimmingCharacters(in: .whitespacesAndNewlines)
                    if !value.isEmpty {
                        features.mechanisms.insert(value)
                    }
                }
            case "bind":
                features.supportsBind = true
            case "session":
                features.supportsSession = true
            default:
                break
            }
        }
        return features
    }

    static func parseMessage(from element: XMPPElement, timestamp: Date? = nil) -> XMPPMessageEvent {
        let stanzaID = element.firstChild(named: "stanza-id")?.attribute("id") ?? element.attribute("id")
        let reactionElement = element.firstChild(named: "reactions")
        let applyTo = element.firstChild(named: "apply-to")
        let moderatedRetraction = applyTo?
            .firstChild(named: "moderated")?
            .firstChild(named: "retract") != nil
        let replyElement = element.firstChild(named: "reply")
        let rawBody = element.firstChild(named: "body")?.text
        let strippedBody = stripReplyFallbackBody(rawBody, from: element)
        let markupSpans = parseMarkupSpans(from: element, strippedBody: strippedBody, rawBody: rawBody)
        let chatState = parseChatState(from: element)
        let displayedMarkerID = element.firstChild(named: "displayed")?.attribute("id")
        let sharedFiles = parseSharedFiles(from: element)
        let broadcastMention = parseBroadcastMention(from: element)
        return XMPPMessageEvent(
            from: element.attribute("from"),
            to: element.attribute("to"),
            type: element.attribute("type"),
            id: element.attribute("id"),
            stanzaID: stanzaID,
            body: strippedBody,
            subject: element.firstChild(named: "subject")?.text,
            thread: element.firstChild(named: "thread")?.text,
            timestamp: timestamp ?? parseDelayStamp(from: element),
            replacesID: element.firstChild(named: "replace")?.attribute("id"),
            retractsID: element.firstChild(named: "retract")?.attribute("id")
                ?? (moderatedRetraction ? applyTo?.attribute("id") : nil),
            reactionTargetID: reactionElement?.attribute("id"),
            reactionEmojis: reactionElement?.children(named: "reaction").map(\.text).filter { !$0.isEmpty } ?? [],
            replyToID: replyElement?.attribute("id"),
            replyToSender: replyElement?.attribute("to"),
            markupSpans: markupSpans,
            chatState: chatState,
            displayedMarkerID: displayedMarkerID,
            sharedFiles: sharedFiles,
            broadcastMention: broadcastMention
        )
    }

    private static func parseSharedFiles(from element: XMPPElement) -> [XMPPSharedFile] {
        var files: [XMPPSharedFile] = []
        for fs in element.children(named: "file-sharing") {
            let disposition = fs.attribute("disposition") ?? "inline"
            let file = fs.firstChild(named: "file")
            let name = file?.firstChild(named: "name")?.text
            let mediaType = file?.firstChild(named: "media-type")?.text
            let sizeText = file?.firstChild(named: "size")?.text
            let size = sizeText.flatMap { Int($0) }
            let widthText = file?.firstChild(named: "width")?.text
            let width = widthText.flatMap { Int($0) }
            let heightText = file?.firstChild(named: "height")?.text
            let height = heightText.flatMap { Int($0) }
            let url = fs.firstChild(named: "sources")?
                .firstChild(named: "url-data")?
                .attribute("target")
            guard let url, !url.isEmpty else { continue }
            files.append(XMPPSharedFile(
                url: url,
                name: name,
                mediaType: mediaType,
                size: size,
                width: width,
                height: height,
                disposition: disposition
            ))
        }
        return files
    }

    private static func parseBroadcastMention(from element: XMPPElement) -> String? {
        guard let mentions = element.firstChild(named: "mentions") else { return nil }
        for mention in mentions.children(named: "mention") {
            if let type = mention.attribute("type"), (type == "everyone" || type == "here") {
                return type
            }
        }
        return nil
    }

    private static func parseChatState(from element: XMPPElement) -> String? {
        let chatStates: Set<String> = ["active", "composing", "paused", "inactive", "gone"]
        for child in element.children {
            if chatStates.contains(child.localName) {
                return child.localName
            }
        }
        return nil
    }

    private static func parseMarkupSpans(from element: XMPPElement, strippedBody: String?, rawBody: String?) -> [XMPPMarkupSpan] {
        guard let markup = element.firstChild(named: "markup") else { return [] }
        let fallbackOffset = replyFallbackOffset(from: element, rawBody: rawBody)
        var spans: [XMPPMarkupSpan] = []
        for child in markup.children {
            guard let spanType = XMPPMarkupSpan.SpanType(rawValue: child.localName) else { continue }
            guard let startStr = child.attribute("start"), let start = Int(startStr),
                  let endStr = child.attribute("end"), let end = Int(endStr),
                  start >= 0, end > start else { continue }
            let adjustedStart = max(0, start - fallbackOffset)
            let adjustedEnd = max(0, end - fallbackOffset)
            guard adjustedEnd > adjustedStart else { continue }
            spans.append(XMPPMarkupSpan(
                type: spanType,
                start: adjustedStart,
                end: adjustedEnd,
                uri: child.attribute("uri")
            ))
        }
        return spans
    }

    private static func replyFallbackOffset(from element: XMPPElement, rawBody: String?) -> Int {
        guard let rawBody else { return 0 }
        for fallback in element.children(named: "fallback") {
            guard fallback.attribute("for") == "urn:xmpp:reply:0" else { continue }
            guard let bodyRange = fallback.firstChild(named: "body") else { continue }
            let end = Int(bodyRange.attribute("end") ?? "0") ?? 0
            guard end > 0, end <= rawBody.utf16.count else { continue }
            return end
        }
        return 0
    }

    private static func stripReplyFallbackBody(_ body: String?, from element: XMPPElement) -> String? {
        guard let body, !body.isEmpty else { return body }
        for fallback in element.children(named: "fallback") {
            guard fallback.attribute("for") == "urn:xmpp:reply:0" else { continue }
            guard let bodyRange = fallback.firstChild(named: "body") else { continue }
            let start = Int(bodyRange.attribute("start") ?? "0") ?? 0
            let end = Int(bodyRange.attribute("end") ?? "0") ?? 0
            guard start >= 0, end > start, end <= body.utf16.count else { continue }
            let utf16 = body.utf16
            let endIndex = utf16.index(utf16.startIndex, offsetBy: end)
            return String(body[endIndex...])
        }
        return body
    }

    static func parseMamResult(from element: XMPPElement) -> XMPPArchiveMessage? {
        guard element.localName == "message",
              let result = element.firstChild(named: "result"),
              let forwarded = result.firstChild(named: "forwarded"),
              let message = forwarded.firstChild(named: "message") else {
            return nil
        }

        let archiveID = result.attribute("id")
        let queryID = result.attribute("queryid")
        let timestamp = parseDelayStamp(from: forwarded)
        let parsedMessage = parseMessage(from: message, timestamp: timestamp)
        return XMPPArchiveMessage(
            mamID: archiveID,
            queryID: queryID,
            stanzaID: parsedMessage.stanzaID,
            delayedDeliveryTimestamp: timestamp,
            message: parsedMessage
        )
    }

    static func parsePresence(from element: XMPPElement) -> XMPPPresenceEvent {
        XMPPPresenceEvent(
            from: element.attribute("from"),
            to: element.attribute("to"),
            type: element.attribute("type"),
            status: element.firstChild(named: "status")?.text,
            show: element.firstChild(named: "show")?.text
        )
    }

    static func parseBoundJID(from element: XMPPElement) -> String? {
        element
            .firstChild(named: "bind")?
            .firstChild(named: "jid")?
            .text
    }

    static func parseMamPageInfo(from element: XMPPElement) -> XMPPRSMPageInfo {
        guard let fin = element.firstChild(named: "fin") else {
            return XMPPRSMPageInfo(first: nil, last: nil, count: nil, index: nil, isComplete: false)
        }

        return parseRSMPageInfo(from: fin.firstChild(named: "set"), isComplete: parseBooleanAttribute(fin.attribute("complete")))
    }

    static func streamError(from element: XMPPElement) -> (name: String, text: String?)? {
        guard element.localName == "error" else {
            return nil
        }
        let condition = element.children.first?.localName ?? "error"
        return (condition, element.text.isEmpty ? nil : element.text)
    }

    static func parseDiscoItems(from element: XMPPElement) -> [XMPPDiscoItem] {
        guard element.localName == "iq",
              let query = element.firstChild(named: "query") else {
            return []
        }

        return query.children(named: "item").map { item in
            XMPPDiscoItem(
                jid: item.attribute("jid"),
                name: item.attribute("name"),
                node: item.attribute("node")
            )
        }
    }

    static func discoIdentityName(from element: XMPPElement) -> String? {
        element
            .firstChild(named: "query")?
            .children(named: "identity")
            .first?.attribute("name")
    }

    static func discoFeatures(from element: XMPPElement) -> Set<String> {
        Set(
            element
                .firstChild(named: "query")?
                .children(named: "feature")
                .compactMap { $0.attribute("var") } ?? []
        )
    }

    static func discoFieldValue(from element: XMPPElement, named fieldName: String) -> String? {
        guard let query = element.firstChild(named: "query") else {
            return nil
        }

        for form in query.children where form.localName == "x" {
            for field in form.children(named: "field") where field.attribute("var") == fieldName {
                if let value = field.children(named: "value").first?.text,
                   !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    return value.trimmingCharacters(in: .whitespacesAndNewlines)
                }
                if !field.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    return field.text.trimmingCharacters(in: .whitespacesAndNewlines)
                }
            }
        }

        return nil
    }

    private static func parseDelayStamp(from element: XMPPElement) -> Date? {
        guard let delay = element.firstChild(named: "delay"),
              let stamp = delay.attribute("stamp") else {
            return nil
        }

        return parseXMPPDate(stamp)
    }

    private static func parseRSMPageInfo(from element: XMPPElement?, isComplete: Bool) -> XMPPRSMPageInfo {
        guard let element else {
            return XMPPRSMPageInfo(first: nil, last: nil, count: nil, index: nil, isComplete: isComplete)
        }

        return XMPPRSMPageInfo(
            first: element.firstChild(named: "first")?.text,
            last: element.firstChild(named: "last")?.text,
            count: Int(element.firstChild(named: "count")?.text ?? ""),
            index: Int(element.firstChild(named: "index")?.text ?? ""),
            isComplete: isComplete
        )
    }

    private static func parseBooleanAttribute(_ value: String?) -> Bool {
        switch value?.lowercased() {
        case "true", "1", "yes":
            return true
        default:
            return false
        }
    }

    private static func parseXMPPDate(_ value: String) -> Date? {
        let formatters: [ISO8601DateFormatter] = {
            let base = ISO8601DateFormatter()
            base.formatOptions = [.withInternetDateTime, .withFractionalSeconds]

            let fallback = ISO8601DateFormatter()
            fallback.formatOptions = [.withInternetDateTime]

            return [base, fallback]
        }()

        for formatter in formatters {
            if let date = formatter.date(from: value) {
                return date
            }
        }

        return nil
    }

    private static func consumeDocument(from buffer: inout String) -> String? {
        guard let openIndex = buffer.firstIndex(of: "<") else {
            buffer.removeAll(keepingCapacity: false)
            return nil
        }

        if openIndex > buffer.startIndex {
            buffer.removeSubrange(buffer.startIndex..<openIndex)
        }

        var index = buffer.startIndex
        var depth = 0
        var insideTag = false
        var inQuote: Character?
        var tagStart: String.Index?
        var currentTagIsClosing = false
        var currentTagIsSpecial = false

        while index < buffer.endIndex {
            let character = buffer[index]

            if let quote = inQuote {
                if character == quote {
                    inQuote = nil
                }
                index = buffer.index(after: index)
                continue
            }

            switch character {
            case "\"", "'":
                inQuote = character
            case "<":
                insideTag = true
                tagStart = index
                let next = nextCharacter(in: buffer, after: index)
                currentTagIsClosing = next == "/"
                currentTagIsSpecial = next == "?" || next == "!"
                if currentTagIsSpecial {
                    if let closing = buffer[index...].firstIndex(of: ">") {
                        index = closing
                    } else {
                        return nil
                    }
                    tagStart = nil
                    insideTag = false
                } else if !currentTagIsClosing {
                    depth += 1
                } else if depth > 0 {
                    depth -= 1
                }
            case ">":
                guard insideTag else { break }
                if let tagStart {
                    let tag = String(buffer[tagStart...index])
                    if !currentTagIsClosing,
                       tag.trimmingCharacters(in: .whitespacesAndNewlines).hasSuffix("/>"),
                       depth > 0 {
                        depth -= 1
                    }
                }

                if depth == 0 {
                    let document = String(buffer[buffer.startIndex...index])
                    buffer.removeSubrange(buffer.startIndex...index)
                    return document
                }

                tagStart = nil
                currentTagIsClosing = false
                currentTagIsSpecial = false
                insideTag = false
            default:
                break
            }

            index = buffer.index(after: index)
        }

        return nil
    }

    private static func trimLeadingWhitespace(_ buffer: inout String) {
        while let first = buffer.first, first.isWhitespace || first.isNewline {
            buffer.removeFirst()
        }
    }

    private static func nextCharacter(in string: String, after index: String.Index) -> Character? {
        let next = string.index(after: index)
        guard next < string.endIndex else {
            return nil
        }
        return string[next]
    }
}
