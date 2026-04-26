/** Waddle GitHub enrichment embeds. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_WADDLE_GITHUB_0 = "urn:waddle:github:0";

const fields = {
  url: attribute("url"),
  owner: attribute("owner"),
  name: attribute("name"),
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.githubRepos", multiple: true }],
    element: "repo",
    fields,
    namespace: NS_WADDLE_GITHUB_0,
  },
  {
    aliases: [{ path: "message.githubIssues", multiple: true }],
    element: "issue",
    fields,
    namespace: NS_WADDLE_GITHUB_0,
  },
  {
    aliases: [{ path: "message.githubPullRequests", multiple: true }],
    element: "pr",
    fields,
    namespace: NS_WADDLE_GITHUB_0,
  },
];

export default definitions;
