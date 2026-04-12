/** XEP-0446/0447: Stateless File Sharing. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childAttribute, childText } from "stanza/jxt";

const NS_SFS_0 = "urn:xmpp:sfs:0";
const NS_FILE_METADATA_0 = "urn:xmpp:file:metadata:0";
const NS_URL_DATA = "http://jabber.org/protocol/url-data";

export interface WaddleFileSharing {
  disposition?: string;
  name?: string;
  mediaType?: string;
  size?: string;
  width?: string;
  height?: string;
  desc?: string;
  url?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.fileSharing", multiple: false }],
    element: "file-sharing",
    fields: {
      disposition: attribute("disposition"),
      name: childText(NS_FILE_METADATA_0, "name"),
      mediaType: childText(NS_FILE_METADATA_0, "media-type"),
      size: childText(NS_FILE_METADATA_0, "size"),
      width: childText(NS_FILE_METADATA_0, "width"),
      height: childText(NS_FILE_METADATA_0, "height"),
      desc: childText(NS_FILE_METADATA_0, "desc"),
      url: childAttribute(NS_URL_DATA, "url-data", "target"),
    },
    namespace: NS_SFS_0,
  },
];

export default definitions;
