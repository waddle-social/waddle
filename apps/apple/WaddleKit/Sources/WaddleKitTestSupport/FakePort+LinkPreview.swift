import WaddleKit

extension FakePort {
    func lookupLinkPreview(_ request: LinkPreviewRequest) async throws -> LinkPreviewLookup {
        linkPreviewRequests.append(request)
        let result = linkPreviewResult
        await linkPreviewLookups.wait()
        return try result.get()
    }
}
