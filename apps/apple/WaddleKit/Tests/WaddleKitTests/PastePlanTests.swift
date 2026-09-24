import Foundation
import Testing
@testable import WaddleKit

struct PastePlanTests {
    private func plan(_ items: [String]...) -> PastePlan {
        PastePlan.plan(items: items.map(PasteboardItem.init(types:)))
    }

    @Test func plainTextIsTextPaste() {
        let result = plan(["public.utf8-plain-text"], ["public.url", "public.utf8-plain-text"])
        #expect(result.loads == [nil, nil])
        #expect(result.isTextPaste)
        #expect(plan().isTextPaste)
    }

    @Test func textItemsNextToAttachmentsPasteToo() {
        let captioned = plan(["public.png"], ["public.utf8-plain-text"], ["public.html"])
        #expect(captioned.loads == [.image(.png), nil, nil])
        #expect(captioned.textItems == [1])
        // A copied file's name is on the file's own item and is not pasted.
        #expect(plan(["public.file-url", "public.utf8-plain-text"]).textItems.isEmpty)
        // A plain text paste leaves the text to the field.
        #expect(plan(["public.utf8-plain-text"]).textItems.isEmpty)
    }

    @Test func gifKeepsAnimation() {
        #expect(plan(["public.png", "com.compuserve.gif", "public.tiff"]).loads == [.gif])
    }

    @Test func bestStillImageWins() {
        #expect(plan(["public.tiff", "public.jpeg", "public.png"]).loads == [.image(.png)])
        #expect(plan(["public.tiff", "public.jpeg"]).loads == [.image(.jpeg)])
        #expect(plan(["public.tiff", "public.heic"]).loads == [.image(.heic)])
        #expect(plan(["public.tiff"]).loads == [.image(.tiff)])
        #expect(!plan(["public.tiff"]).isTextPaste)
    }

    @Test func fileURLIsTheRealFile() {
        #expect(plan(["public.file-url", "public.tiff", "public.utf8-plain-text"]).loads == [.fileURL])
        #expect(plan(["public.file-url", "com.compuserve.gif"]).loads == [.fileURL])
    }

    @Test func imageWithURLIsImage() {
        #expect(plan(["public.url", "public.png"]).loads == [.image(.png)])
        #expect(plan(["public.html", "public.png"]).loads == [.image(.png)])
    }

    @Test func richTextSelectionWithImagePastesAsText() {
        let office = plan(["public.html", "public.utf8-plain-text", "public.png", "public.tiff"])
        #expect(office.loads == [nil])
        #expect(office.isTextPaste)
        #expect(plan(["public.html", "public.plain-text", "public.jpeg"]).isTextPaste)
        // A GIF or file is never mistaken for a text selection.
        #expect(plan(["public.html", "public.utf8-plain-text", "com.compuserve.gif"]).loads == [.gif])
    }

    @Test func mixedItemsLoadPerItem() {
        let result = plan(["public.utf8-plain-text"], ["public.jpeg"], ["com.compuserve.gif"])
        #expect(result.loads == [nil, .image(.jpeg), .gif])
        #expect(!result.isTextPaste)
    }
}
