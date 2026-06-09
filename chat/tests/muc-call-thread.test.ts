import { afterEach, describe, expect, test } from "bun:test";
import {
  $mucCallThreadId,
  forgetMucCallThread,
  readMucCallThread,
  rememberMucCallThread,
} from "../src/lib/calls/muc-call-thread";

describe("muc call-thread store", () => {
  afterEach(() => $mucCallThreadId.set({}));

  test("remembers a call thread id keyed by the normalized room jid", () => {
    rememberMucCallThread("Lobby@MUC.example.com/alice", "thread-1");
    expect(readMucCallThread("lobby@muc.example.com")).toBe("thread-1");
  });

  test("forgets the thread id (e.g. on the call-thread-ended fastening)", () => {
    rememberMucCallThread("lobby@muc.example.com", "thread-1");
    forgetMucCallThread("lobby@muc.example.com");
    expect(readMucCallThread("lobby@muc.example.com")).toBeNull();
  });

  test("ignores an empty room or thread id", () => {
    rememberMucCallThread("", "thread-1");
    rememberMucCallThread("lobby@muc.example.com", "   ");
    expect(readMucCallThread("lobby@muc.example.com")).toBeNull();
  });

  test("reads from a passed-in snapshot for reactive consumers", () => {
    const snapshot = { "lobby@muc.example.com": "thread-9" };
    expect(readMucCallThread("Lobby@MUC.example.com", snapshot)).toBe("thread-9");
  });
});
