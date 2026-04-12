/** XEP-0317: Hats — override stanza's outdated definition (uri/title attrs). */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";
import { NS_HATS_0 } from "stanza/Namespaces";

export interface WaddleHat {
  uri: string;
  title: string;
}

const definitions: DefinitionOptions[] = [
  {
    element: "hat",
    fields: {
      uri: attribute("uri"),
      title: attribute("title"),
    },
    namespace: NS_HATS_0,
    path: "hat",
  },
];

export default definitions;
