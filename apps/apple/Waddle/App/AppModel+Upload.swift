import Foundation

// MARK: - File Upload (XEP-0363)

extension AppModel {
    func uploadAndSendFile(
        data: Data,
        fileName: String,
        mediaType: String,
        replyTo: ChatTimelineMessage? = nil,
        threadRootID: String? = nil
    ) async {
        guard currentRoomJID != nil else {
            errorMessage = "Select a channel before uploading."
            return
        }

        guard let sharedFile = await uploadSharedFile(data: data, fileName: fileName, mediaType: mediaType) else {
            return
        }

        do {
            try await sendMessage(
                "",
                room: chatStore.selectedRoom,
                replyTo: replyTo,
                threadRootID: threadRootID,
                sharedFiles: [sharedFile]
            )
            if replyTo != nil {
                chatStore.setReplyingTo(nil)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func uploadAndSendDmFile(data: Data, fileName: String, mediaType: String, peerJID: String) async {
        guard let sharedFile = await uploadSharedFile(data: data, fileName: fileName, mediaType: mediaType) else {
            return
        }
        await sendDm(body: "", sharedFiles: [sharedFile], peerJID: peerJID)
    }

    private func uploadSharedFile(data: Data, fileName: String, mediaType: String) async -> WaddleSharedFile? {
        guard data.count <= maxUploadFileBytes else {
            let sizeMb = Double(data.count) / 1024.0 / 1024.0
            errorMessage = "File too large (\(String(format: "%.1f", sizeMb)) MB). Maximum upload size is 10 MB."
            return nil
        }

        guard let rustClient else {
            errorMessage = ChatSendError.noSession.errorDescription ?? "Sign in again to reconnect live chat."
            return nil
        }

        isUploadingFile = true
        defer { isUploadingFile = false }

        if uploadServiceJID == nil {
            uploadServiceJID = await rustClient.discoverUploadService()
        }
        guard let serviceJID = uploadServiceJID else {
            errorMessage = "File upload is not available on this server."
            return nil
        }

        guard let slot = await rustClient.requestUploadSlot(
            serviceJID: serviceJID,
            filename: fileName,
            size: data.count,
            contentType: mediaType
        ) else {
            errorMessage = "Failed to request an upload slot."
            return nil
        }

        guard let putURL = URL(string: slot.putURL) else {
            errorMessage = "Upload slot returned an invalid URL."
            return nil
        }

        do {
            var request = URLRequest(url: putURL)
            request.httpMethod = "PUT"
            request.setValue(mediaType, forHTTPHeaderField: "Content-Type")
            for (name, value) in slot.putHeaders {
                request.setValue(value, forHTTPHeaderField: name)
            }

            let (_, response) = try await URLSession.shared.upload(for: request, from: data)
            guard let httpResponse = response as? HTTPURLResponse,
                  (200..<300).contains(httpResponse.statusCode) else {
                errorMessage = "File upload failed."
                return nil
            }
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }

        return WaddleSharedFile(
            url: slot.getURL,
            name: fileName,
            mediaType: mediaType,
            size: UInt64(data.count),
            width: nil,
            height: nil,
            desc: nil,
            hashes: [],
            disposition: sharedFileDisposition(for: mediaType),
            encrypted: nil
        )
    }

    private func sharedFileDisposition(for mediaType: String) -> String {
        if mediaType.hasPrefix("image/")
            || mediaType.hasPrefix("video/")
            || mediaType.hasPrefix("audio/")
            || mediaType == "application/pdf" {
            return "inline"
        }
        return "attachment"
    }

    func timelineSharedFile(from file: WaddleSharedFile) -> XMPPSharedFile {
        XMPPSharedFile(
            url: file.url,
            name: file.name,
            mediaType: file.mediaType,
            size: file.size.flatMap(Int.init),
            width: file.width.flatMap(Int.init),
            height: file.height.flatMap(Int.init),
            disposition: file.disposition,
            encryptedSource: nil
        )
    }
}
