import { describe, expect, test } from "bun:test";
import { parseSlashTrigger } from "../src/lib/slash-trigger";

describe("parseSlashTrigger", () => {
  test("returns null when text does not start with slash", () => {
    expect(parseSlashTrigger("hello")).toBeNull();
  });

  test("returns null for empty input", () => {
    expect(parseSlashTrigger("")).toBeNull();
  });

  test("returns empty prefix and trailing when text is just slash", () => {
    expect(parseSlashTrigger("/")).toEqual({ prefix: "", trailing: "" });
  });

  test("captures prefix while user is typing", () => {
    expect(parseSlashTrigger("/ai")).toEqual({ prefix: "ai", trailing: "" });
  });

  test("splits prefix and trailing on first whitespace", () => {
    expect(parseSlashTrigger("/ai hello world")).toEqual({ prefix: "ai", trailing: "hello world" });
  });

  test("collapses leading whitespace before trailing text", () => {
    expect(parseSlashTrigger("/ai   hello")).toEqual({ prefix: "ai", trailing: "hello" });
  });

  test("rejects non-letter first character after slash", () => {
    expect(parseSlashTrigger("/123")).toBeNull();
    expect(parseSlashTrigger("/-foo")).toBeNull();
  });

  test("rejects illegal characters within prefix", () => {
    expect(parseSlashTrigger("/ai!")).toBeNull();
  });

  test("allows hyphen and underscore inside the prefix", () => {
    expect(parseSlashTrigger("/quick-poll question")).toEqual({ prefix: "quick-poll", trailing: "question" });
    expect(parseSlashTrigger("/my_cmd")).toEqual({ prefix: "my_cmd", trailing: "" });
  });

  test("treats slash plus whitespace as empty prefix with no trailing", () => {
    expect(parseSlashTrigger("/ ")).toEqual({ prefix: "", trailing: "" });
  });

  test("does not match double slash", () => {
    expect(parseSlashTrigger("//foo")).toBeNull();
  });
});
