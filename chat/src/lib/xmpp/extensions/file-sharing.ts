/**
 * XEP-0446/0447: Stateless File Sharing.
 *
 * XML structure:
 *   <file-sharing xmlns="urn:xmpp:sfs:0" disposition="inline">
 *     <file xmlns="urn:xmpp:file:metadata:0">
 *       <name>photo.jpg</name>
 *       <media-type>image/jpeg</media-type>
 *       <size>12345</size>
 *       ...
 *     </file>
 *     <sources>
 *       <url-data xmlns="http://jabber.org/protocol/url-data" target="https://..."/>
 *     </sources>
 *   </file-sharing>
 */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import { attribute, deepChildText } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";

const NS_SFS_0 = "urn:xmpp:sfs:0";
const NS_FILE_METADATA_0 = "urn:xmpp:file:metadata:0";
const NS_URL_DATA = "http://jabber.org/protocol/url-data";


function fileField(element: string): FieldDefinition<string> {
  return deepChildText([
    { namespace: NS_FILE_METADATA_0, element: "file" },
    { namespace: NS_FILE_METADATA_0, element },
  ]);
}

/** Custom field: reads target attr from <sources><url-data target="..."/></sources>. */
const urlField: FieldDefinition<string> = {
  importer(xml: XMLElement) {
    const sources = xml.getChild("sources", NS_SFS_0);
    if (!sources) return undefined;
    const urlData = sources.getChild("url-data", NS_URL_DATA);
    return urlData?.getAttribute("target") ?? undefined;
  },
  exporter(xml: XMLElement, value: string) {
    if (!value) return;
    let sources = xml.getChild("sources", NS_SFS_0);
    if (!sources) {
      sources = new XMLElement("sources", { xmlns: NS_SFS_0 });
      xml.appendChild(sources);
    }
    let urlData = sources.getChild("url-data", NS_URL_DATA);
    if (!urlData) {
      urlData = new XMLElement("url-data", { xmlns: NS_URL_DATA });
      sources.appendChild(urlData);
    }
    urlData.setAttribute("target", value);
  },
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.fileSharing", multiple: true }],
    element: "file-sharing",
    fields: {
      disposition: attribute("disposition"),
      name: fileField("name"),
      mediaType: fileField("media-type"),
      size: fileField("size"),
      width: fileField("width"),
      height: fileField("height"),
      desc: fileField("desc"),
      url: urlField,
    },
    namespace: NS_SFS_0,
  },
];

export default definitions;
