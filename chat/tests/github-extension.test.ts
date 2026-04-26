import { describe, expect, test } from "bun:test";
import { Registry, XMLElement } from "stanza/jxt";
import githubDefinitions from "../src/lib/xmpp/extensions/github";

const NS_WADDLE_GITHUB_0 = "urn:waddle:github:0";

function newRegistry(): Registry {
  const registry = new Registry();
  registry.define(githubDefinitions);
  return registry;
}

describe("GitHub enrichment jxt extension", () => {
  test("imports repo, issue, and pull request XML payloads", () => {
    const registry = newRegistry();
    const repo = new XMLElement("repo", {
      xmlns: NS_WADDLE_GITHUB_0,
      url: "https://github.com/waddle-social/waddle",
      owner: "waddle-social",
      name: "waddle",
    });
    const issue = new XMLElement("issue", {
      xmlns: NS_WADDLE_GITHUB_0,
      url: "https://github.com/waddle-social/waddle/issues/42",
      owner: "waddle-social",
      name: "waddle",
    });
    const pr = new XMLElement("pr", {
      xmlns: NS_WADDLE_GITHUB_0,
      url: "https://github.com/waddle-social/waddle/pull/48",
      owner: "waddle-social",
      name: "waddle",
    });

    expect(registry.import(repo)).toEqual({
      url: "https://github.com/waddle-social/waddle",
      owner: "waddle-social",
      name: "waddle",
    });
    expect(registry.import(issue)).toEqual({
      url: "https://github.com/waddle-social/waddle/issues/42",
      owner: "waddle-social",
      name: "waddle",
    });
    expect(registry.import(pr)).toEqual({
      url: "https://github.com/waddle-social/waddle/pull/48",
      owner: "waddle-social",
      name: "waddle",
    });
  });

  test("exports repo, issue, and pull request XML payloads", () => {
    const registry = newRegistry();
    const repo = registry.export("message.githubRepos", {
      url: "https://github.com/waddle-social/waddle",
      owner: "waddle-social",
      name: "waddle",
    } as unknown as Parameters<Registry["export"]>[1]);
    const issue = registry.export("message.githubIssues", {
      url: "https://github.com/waddle-social/waddle/issues/42",
      owner: "waddle-social",
      name: "waddle",
    } as unknown as Parameters<Registry["export"]>[1]);
    const pr = registry.export("message.githubPullRequests", {
      url: "https://github.com/waddle-social/waddle/pull/48",
      owner: "waddle-social",
      name: "waddle",
    } as unknown as Parameters<Registry["export"]>[1]);

    expect(repo?.name).toBe("repo");
    expect(repo?.getNamespace()).toBe(NS_WADDLE_GITHUB_0);
    expect(repo?.getAttribute("url")).toBe("https://github.com/waddle-social/waddle");
    expect(issue?.name).toBe("issue");
    expect(issue?.getAttribute("url")).toBe("https://github.com/waddle-social/waddle/issues/42");
    expect(pr?.name).toBe("pr");
    expect(pr?.getAttribute("url")).toBe("https://github.com/waddle-social/waddle/pull/48");
  });
});
