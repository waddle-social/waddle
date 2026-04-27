/** Waddle unified extension framework envelope. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childText, text } from "stanza/jxt";

const NS_WADDLE_EXTENSION_1 = "urn:waddle:extension:1";
const NS_LINKS_TASK_BOARD_1 = "urn:waddle:links-task-board:1";
const NS_PUB_QUIZ_1 = "urn:waddle:pub-quiz:1";
const NS_AI_CHATBOT_1 = "urn:waddle:ai-chatbot:1";
const NS_AI_ASSISTANT_CANVAS_1 = "urn:waddle:ai-assistant-canvas:1";
const NS_DECISION_POLLS_1 = "urn:waddle:decision-polls:1";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.waddleExtensions", multiple: false }],
    element: "extensions",
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments", multiple: true }],
    element: "enrichment",
    fields: {
      id: attribute("id"),
      plugin: attribute("plugin"),
      capability: attribute("capability"),
      payloadNamespace: attribute("payload-ns"),
      created: attribute("created"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload", multiple: false }],
    element: "payload",
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.links", multiple: true }],
    element: "link",
    fields: {
      url: attribute("url"),
      title: attribute("title"),
      site: attribute("site"),
    },
    namespace: NS_LINKS_TASK_BOARD_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.quizQuestions", multiple: true }],
    element: "quiz-question",
    fields: {
      gameId: attribute("game-id"),
      questionId: attribute("question-id"),
    },
    namespace: NS_PUB_QUIZ_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.assistantAnswers", multiple: true }],
    element: "assistant-answer",
    fields: {
      runId: attribute("run-id"),
      profile: attribute("profile"),
    },
    namespace: NS_AI_CHATBOT_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.canvases", multiple: true }],
    element: "canvas",
    fields: {
      canvasId: attribute("canvas-id"),
      renderId: attribute("render-id"),
    },
    namespace: NS_AI_ASSISTANT_CANVAS_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.polls", multiple: true }],
    element: "poll",
    fields: {
      pollId: attribute("poll-id"),
      mode: attribute("mode"),
    },
    namespace: NS_DECISION_POLLS_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.views", multiple: true }],
    element: "view",
    fields: {
      id: attribute("id"),
      title: attribute("title"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.views.textBlocks", multiple: true }],
    element: "text",
    fields: {
      style: attribute("style"),
      text: text(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.launches", multiple: true }],
    element: "launch",
    fields: {
      id: attribute("id"),
      plugin: attribute("plugin"),
      action: attribute("action"),
      commandNode: attribute("command-node"),
      label: attribute("label"),
      expiresAt: attribute("expires-at"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations", multiple: true }],
    element: "annotation",
    fields: {
      extension: attribute("extension"),
      id: attribute("id"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations.card", multiple: false }],
    element: "card",
    fields: {
      title: childText(null, "title"),
      summary: childText(null, "summary"),
      image: childText(null, "image"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations.card.fields", multiple: true }],
    element: "field",
    fields: {
      name: attribute("name"),
      value: text(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations.card.actions", multiple: true }],
    element: "action",
    fields: {
      route: attribute("route"),
      label: text(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
];

export default definitions;
