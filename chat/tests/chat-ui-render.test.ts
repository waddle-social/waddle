import { describe, expect, test } from "bun:test";
import {
  extensionCardDetails,
  extensionPresentation,
  inferredFileDisposition,
  isAudioFile,
  isImageFile,
  isPdfFile,
  isVideoFile,
  renderStyledBody,
  type ExtensionAnnotation,
  type MarkupSpan,
  type MessageReference,
} from "../src/lib/chat-ui";

describe("renderStyledBody", () => {
  test("renders plain text literally instead of parsing Markdown", () => {
    expect(renderStyledBody("Hello **again**")).toBe("<p>Hello **again**</p>");
    expect(renderStyledBody("# heading")).toBe("<p># heading</p>");
    expect(renderStyledBody("![alt](https://example.com/image.png)")).toBe("<p>![alt](https://example.com/image.png)</p>");
  });

  test("renders inline XEP-0394 spans with code point offsets", () => {
    const body = "Hi 👋 world";
    const markup: MarkupSpan[] = [
      { type: "span", start: 5, end: 10, styles: ["strong"] },
      { type: "span", start: 3, end: 4, styles: ["code"] },
    ];

    const html = renderStyledBody(body, markup);

    expect(html).toContain("Hi <code");
    expect(html).toContain("<strong>world</strong>");
  });

  test("renders code blocks, blockquotes, and lists from markup metadata", () => {
    const code = renderStyledBody("const x = 1", [
      { type: "bcode", start: 0, end: 11, language: "ts" },
    ]);
    const quote = renderStyledBody("> quoted", [
      { type: "bquote", start: 0, end: 8 },
    ]);
    const list = renderStyledBody("- one\n- two", [
      { type: "list", start: 0, end: 11, ordered: false, items: [0, 6] },
    ]);

    expect(code).toContain('data-code-block="true"');
    expect(code).toContain('data-language="ts"');
    expect(quote).toContain("<blockquote");
    expect(quote).toContain("<p>quoted</p>");
    expect(list).toContain("<ul>");
    expect(list).toContain("<li><p>one</p></li>");
    expect(list).toContain("<li><p>two</p></li>");
    expect(list).not.toContain("- one");
  });

  test("renders ordered lists and links through XEP-0372 references", () => {
    const body = "3. docs\n4. more";
    const markup: MarkupSpan[] = [
      { type: "list", start: 0, end: body.length, ordered: true, items: [0, 8] },
    ];
    const references: MessageReference[] = [
      { type: "data", uri: "https://example.com/docs", begin: 3, end: 7 },
    ];

    const html = renderStyledBody(body, markup, references);

    expect(html).toContain('<ol start="3">');
    expect(html).toContain('<a href="https://example.com/docs"');
    expect(html).toContain(">docs</a>");
  });

  test("renders bare safe URLs as links when archived messages have no references", () => {
    const html = renderStyledBody("see https://example.com today");

    expect(html).toContain('<a href="https://example.com/"');
    expect(html).toContain(">https://example.com</a>");
  });

  test("does not autolink bare URLs inside code markup", () => {
    const html = renderStyledBody("`https://example.com`", [
      { type: "span", start: 1, end: 20, styles: ["code"] },
    ]);

    expect(html).toContain("<code>https://example.com</code>");
    expect(html).not.toContain("<a ");
  });

  test("escapes unsafe HTML and rejects unsafe links", () => {
    const html = renderStyledBody("<script>x</script>", undefined, [
      { type: "data", uri: "javascript:alert(1)", begin: 0, end: 8 },
    ]);

    expect(html).toBe("<p>&lt;script&gt;x&lt;/script&gt;</p>");
    expect(html).not.toContain("javascript:");
  });

  test("identifies image attachments from media type or URL", () => {
    expect(isImageFile("image/jpeg")).toBe(true);
    expect(isImageFile(undefined, "https://cdn.example.com/cat.PNG?token=1")).toBe(true);
    expect(isImageFile(undefined, "https://media2.giphy.com/media/abc123/200w")).toBe(true);
    expect(isImageFile("application/octet-stream", "https://cdn.example.com/archive.bin")).toBe(false);
  });

  test("identifies video, audio, and PDF attachments and infers disposition", () => {
    expect(isVideoFile("video/mp4")).toBe(true);
    expect(isVideoFile(undefined, "clip.webm")).toBe(true);
    expect(isAudioFile("audio/mpeg")).toBe(true);
    expect(isAudioFile(undefined, "https://cdn.example.com/theme.ogg")).toBe(true);
    expect(isPdfFile("application/pdf")).toBe(true);
    expect(isPdfFile(undefined, "notes.pdf")).toBe(true);
    expect(inferredFileDisposition("text/plain", "notes.txt")).toBe("attachment");
    expect(inferredFileDisposition("video/mp4", "clip.mp4")).toBe("inline");
  });

  test("summarizes generic extension payload cards without sample-specific types", () => {
    const annotation: ExtensionAnnotation = {
      extensionId: "decision-polls",
      annotationId: "poll-1",
      surfaceKind: "utility-panel",
      title: "Ship the extension framework this week?",
      summary: "urn:waddle:decision-polls:1",
      payloadNamespace: "urn:waddle:decision-polls:1",
      fields: {
        capability: "launch",
        payloadNamespace: "urn:waddle:decision-polls:1",
      },
      payloads: [{
        namespace: "urn:waddle:decision-polls:1",
        name: "poll",
        attributes: {
          xmlns: "urn:waddle:decision-polls:1",
          "poll-id": "poll-1",
          mode: "single",
          "closes-at": "2026-04-27T11:00:00Z",
        },
        children: [
          {
            namespace: "urn:waddle:decision-polls:1",
            name: "question",
            attributes: {},
            text: "Ship the extension framework this week?",
            children: [],
          },
          {
            namespace: "urn:waddle:decision-polls:1",
            name: "option",
            attributes: { id: "yes" },
            text: "Yes",
            children: [],
          },
        ],
      }],
      actions: [],
    };

    expect(extensionCardDetails(annotation)).toEqual([
      { label: "Capability", value: "launch" },
      { label: "Mode", value: "single" },
      { label: "Closes At", value: "2026-04-27T11:00:00Z" },
      { label: "Question", value: "Ship the extension framework this week?" },
      { label: "Option", value: "Yes" },
    ]);
  });

  test("renders decision poll extensions through generic presentations", () => {
    const annotation: ExtensionAnnotation = {
      extensionId: "decision-polls",
      annotationId: "poll-1",
      surfaceKind: "utility-panel",
      title: "Ship it?",
      summary: "urn:waddle:decision-polls:1",
      payloadNamespace: "urn:waddle:decision-polls:1",
      fields: {
        payloadNamespace: "urn:waddle:decision-polls:1",
      },
      payloads: [{
        namespace: "urn:waddle:decision-polls:1",
        name: "poll",
        attributes: {
          xmlns: "urn:waddle:decision-polls:1",
          status: "open",
        },
        children: [
          {
            namespace: "urn:waddle:decision-polls:1",
            name: "question",
            attributes: {},
            text: "Ship it?",
            children: [],
          },
          {
            namespace: "urn:waddle:decision-polls:1",
            name: "option",
            attributes: { id: "yes", votes: "2" },
            text: "Yes",
            children: [],
          },
          {
            namespace: "urn:waddle:decision-polls:1",
            name: "option",
            attributes: { id: "no", votes: "1" },
            text: "No",
            children: [],
          },
        ],
      }],
      actions: [],
    };

    expect(extensionPresentation(annotation)).toMatchObject({
      kind: "generic",
      label: "Utility",
      title: "Ship it?",
      options: [
        { id: "yes", label: "Yes", value: 2 },
        { id: "no", label: "No", value: 1 },
      ],
    });
    expect(extensionPresentation(annotation).summary).toBeUndefined();
  });

  test("renders poll options stored as payload attributes without exposing internal fields", () => {
    const annotation: ExtensionAnnotation = {
      extensionId: "decision-polls",
      annotationId: "poll-1",
      surfaceKind: "utility-panel",
      title: "Ship it?",
      summary: "urn:waddle:decision-polls:1",
      payloadNamespace: "urn:waddle:decision-polls:1",
      fields: {
        payloadNamespace: "urn:waddle:decision-polls:1",
      },
      payloads: [{
        namespace: "urn:waddle:decision-polls:1",
        name: "poll",
        attributes: {
          xmlns: "urn:waddle:decision-polls:1",
          "poll-id": "poll-1",
          "option-count": "2",
          "option-0-id": "yes",
          "option-0-label": "Yes",
          "option-1-id": "no",
          "option-1-label": "No",
        },
        children: [],
      }],
      actions: [],
    };

    expect(extensionPresentation(annotation)).toMatchObject({
      kind: "generic",
      options: [
        { id: "yes", label: "Yes" },
        { id: "no", label: "No" },
      ],
      details: [],
    });
  });

  test("renders active extension payloads through generic surface presentations", () => {
    const chatbotAnnotation: ExtensionAnnotation = {
      extensionId: "ai-chatbot",
      annotationId: "chatbot-1",
      surfaceKind: "chat-bot",
      title: "Answer card",
      summary: "urn:waddle:ai-chatbot:1",
      payloadNamespace: "urn:waddle:ai-chatbot:1",
      fields: {
        payloadNamespace: "urn:waddle:ai-chatbot:1",
      },
      payloads: [{
        namespace: "urn:waddle:ai-chatbot:1",
        name: "assistant-answer",
        attributes: {
          xmlns: "urn:waddle:ai-chatbot:1",
        },
        children: [{
          namespace: "urn:waddle:ai-chatbot:1",
          name: "answer",
          attributes: {},
          text: "Use the generic card renderer.",
          children: [],
        }],
      }],
      actions: [],
    };
    const linkAnnotation: ExtensionAnnotation = {
      extensionId: "link-board",
      annotationId: "link-1",
      surfaceKind: "message-card",
      title: "Link Board",
      summary: "urn:waddle:link-board:1",
      payloadNamespace: "urn:waddle:link-board:1",
      fields: {
        payloadNamespace: "urn:waddle:link-board:1",
      },
      payloads: [{
        namespace: "urn:waddle:link-board:1",
        name: "link",
        attributes: {
          xmlns: "urn:waddle:link-board:1",
          url: "https://example.org/post",
        },
        text: "https://example.org/post",
        children: [],
      }],
      actions: [],
    };

    expect(extensionPresentation(chatbotAnnotation)).toMatchObject({
      kind: "generic",
      label: "Chat bot",
      title: "Answer card",
      summary: "Use the generic card renderer.",
      details: [{ label: "Answer", value: "Use the generic card renderer." }],
    });
    expect(extensionPresentation(linkAnnotation)).toMatchObject({
      kind: "generic",
      label: "Extension",
      title: "Link Board",
      summary: "https://example.org/post",
      details: [{ label: "Url", value: "https://example.org/post" }],
    });
  });

  test("does not use raw payload namespace as generic summary", () => {
    const annotation: ExtensionAnnotation = {
      extensionId: "unknown-extension",
      annotationId: "generic-1",
      surfaceKind: "message-card",
      title: "Generic card",
      summary: "urn:waddle:unknown-extension:1",
      payloadNamespace: "urn:waddle:unknown-extension:1",
      fields: {
        payloadNamespace: "urn:waddle:unknown-extension:1",
      },
      payloads: [{
        namespace: "urn:waddle:unknown-extension:1",
        name: "payload",
        attributes: {},
        children: [],
      }],
      actions: [],
    };

    expect(extensionPresentation(annotation)).toMatchObject({
      kind: "generic",
      title: "Generic card",
    });
    expect(extensionPresentation(annotation).summary).toBeUndefined();
  });

  test("renders GitHub webhook payloads as status cards", () => {
    const annotation: ExtensionAnnotation = {
      extensionId: "github",
      annotationId: "github-delivery-1",
      surfaceKind: "message-card",
      title: "GitHub",
      summary: "GitHub waddle-social/waddle: ci completed with failure",
      payloadNamespace: "urn:waddle:web-integration:1",
      fields: {
        payloadNamespace: "urn:waddle:web-integration:1",
      },
      payloads: [{
        namespace: "urn:waddle:web-integration:1",
        name: "github-event",
        attributes: {
          "event-type": "workflow_run",
          action: "completed",
          repository: "waddle-social/waddle",
          conclusion: "failure",
          name: "ci",
          branch: "main",
          revision: "1234567890abcdef",
          url: "https://github.com/waddle-social/waddle/actions/runs/1",
        },
        children: [],
      }],
      actions: [],
    };

    expect(extensionPresentation(annotation)).toMatchObject({
      kind: "github-event",
      tone: "danger",
      label: "GitHub",
      title: "ci",
      primaryValue: "Failure",
      primaryUrl: "https://github.com/waddle-social/waddle/actions/runs/1",
      details: [
        { label: "Repository", value: "waddle-social/waddle" },
        { label: "Branch", value: "main" },
        { label: "Commit", value: "1234567" },
        { label: "Event", value: "Workflow Run" },
      ],
    });

    annotation.payloads![0]!.attributes.conclusion = "error";
    expect(extensionPresentation(annotation).tone).toBe("danger");

    annotation.payloads![0]!.attributes.url = "javascript:alert(1)";
    expect(extensionPresentation(annotation).primaryUrl).toBeUndefined();

    annotation.payloads![0]!.attributes.url = "HTTPS://github.com/waddle-social/waddle/actions/runs/2";
    expect(extensionPresentation(annotation).primaryUrl).toBe("HTTPS://github.com/waddle-social/waddle/actions/runs/2");
  });
});
