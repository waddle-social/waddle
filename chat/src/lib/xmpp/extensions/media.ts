import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

export const NS_WADDLE_MEDIA_0 = "urn:waddle:media:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "iq.media", multiple: false }],
    element: "media",
    fields: {
      waddle: attribute("waddle"),
      channel: attribute("channel"),
      type: attribute("type"),
      backend: attribute("backend"),
      room: attribute("room"),
      participant: attribute("participant"),
      name: attribute("name"),
      url: attribute("url"),
      token: attribute("token"),
      expires: attribute("expires"),
      canPublish: attribute("can-publish"),
      canPublishData: attribute("can-publish-data"),
      canSubscribe: attribute("can-subscribe"),
    },
    namespace: NS_WADDLE_MEDIA_0,
  },
];

export default definitions;
