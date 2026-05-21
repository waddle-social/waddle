import { describe, expect, test } from "bun:test";
import { outboundCalls, newCallSid, type CallWireSender } from "../src/lib/calls/outbound";

const audioVideo = { audio: true, video: true };

function recorder() {
  const calls: { method: string; args: unknown[] }[] = [];
  const track =
    (method: string) =>
    (...args: unknown[]) => {
      calls.push({ method, args });
      return Promise.resolve();
    };
  const client: CallWireSender = {
    send_call_propose: track("send_call_propose") as CallWireSender["send_call_propose"],
    send_call_proceed: track("send_call_proceed") as CallWireSender["send_call_proceed"],
    send_call_reject: track("send_call_reject") as CallWireSender["send_call_reject"],
    send_call_reject_tie_break: track("send_call_reject_tie_break") as CallWireSender["send_call_reject_tie_break"],
    send_call_retract: track("send_call_retract") as CallWireSender["send_call_retract"],
    send_call_retract_tie_break: track("send_call_retract_tie_break") as CallWireSender["send_call_retract_tie_break"],
    send_call_finish: track("send_call_finish") as CallWireSender["send_call_finish"],
    send_call_finish_migrated: track("send_call_finish_migrated") as CallWireSender["send_call_finish_migrated"],
    send_call_session_initiate: track("send_call_session_initiate") as CallWireSender["send_call_session_initiate"],
    send_call_session_accept: track("send_call_session_accept") as CallWireSender["send_call_session_accept"],
    send_call_session_terminate: track("send_call_session_terminate") as CallWireSender["send_call_session_terminate"],
  };
  return { client, calls };
}

describe("outboundCalls", () => {
  test("propose forwards bare jid + sid + media flags", async () => {
    const { client, calls } = recorder();
    await outboundCalls.propose(client, "bob@waddle.test", "c1", audioVideo);
    expect(calls).toEqual([
      { method: "send_call_propose", args: ["bob@waddle.test", "c1", true, true] },
    ]);
  });

  test("proceed / reject / finish forward full jid; retract accepts generic jid", async () => {
    const { client, calls } = recorder();
    await outboundCalls.proceed(client, "bob@waddle.test/desktop", "c1");
    await outboundCalls.reject(client, "bob@waddle.test/desktop", "c1");
    await outboundCalls.retract(client, "bob@waddle.test", "c1");
    await outboundCalls.finish(client, "bob@waddle.test/desktop", "c1");
    expect(calls).toEqual([
      { method: "send_call_proceed", args: ["bob@waddle.test/desktop", "c1"] },
      { method: "send_call_reject", args: ["bob@waddle.test/desktop", "c1"] },
      { method: "send_call_retract", args: ["bob@waddle.test", "c1"] },
      { method: "send_call_finish", args: ["bob@waddle.test/desktop", "c1"] },
    ]);
  });

  test("tie-break helpers forward full jid and sid", async () => {
    const { client, calls } = recorder();
    await outboundCalls.rejectTieBreak(client, "bob@waddle.test/desktop", "c-loser");
    await outboundCalls.retractTieBreak(client, "bob@waddle.test/desktop", "c-loser");
    await outboundCalls.finishMigrated(client, "bob@waddle.test/desktop", "c-old", "c-new");
    expect(calls).toEqual([
      {
        method: "send_call_reject_tie_break",
        args: ["bob@waddle.test/desktop", "c-loser"],
      },
      {
        method: "send_call_retract_tie_break",
        args: ["bob@waddle.test/desktop", "c-loser"],
      },
      {
        method: "send_call_finish_migrated",
        args: ["bob@waddle.test/desktop", "c-old", "c-new"],
      },
    ]);
  });

  test("sessionInitiate splits media into audio/video booleans", async () => {
    const { client, calls } = recorder();
    await outboundCalls.sessionInitiate(
      client,
      "bob@waddle.test/desktop",
      "alice@waddle.test/desktop",
      "c1",
      { audio: true, video: false },
    );
    expect(calls).toEqual([
      {
        method: "send_call_session_initiate",
        args: ["bob@waddle.test/desktop", "alice@waddle.test/desktop", "c1", true, false],
      },
    ]);
  });

  test("sessionAccept passes responder + media", async () => {
    const { client, calls } = recorder();
    await outboundCalls.sessionAccept(
      client,
      "alice@waddle.test/desktop",
      "bob@waddle.test/desktop",
      "c1",
      audioVideo,
    );
    expect(calls).toEqual([
      {
        method: "send_call_session_accept",
        args: [
          "alice@waddle.test/desktop",
          "bob@waddle.test/desktop",
          "c1",
          true,
          true,
        ],
      },
    ]);
  });

  test("sessionTerminate passes reason through (including null)", async () => {
    const { client, calls } = recorder();
    await outboundCalls.sessionTerminate(
      client,
      "bob@waddle.test/desktop",
      "c1",
      "success",
    );
    await outboundCalls.sessionTerminate(
      client,
      "bob@waddle.test/desktop",
      "c1",
      null,
    );
    expect(calls).toEqual([
      {
        method: "send_call_session_terminate",
        args: ["bob@waddle.test/desktop", "c1", "success"],
      },
      {
        method: "send_call_session_terminate",
        args: ["bob@waddle.test/desktop", "c1", null],
      },
    ]);
  });

  test("throws WasmCallApiUnavailable when the wasm method is missing", async () => {
    const empty: CallWireSender = {};
    await expect(outboundCalls.propose(empty, "x@test", "c1", audioVideo)).rejects.toThrow(
      /send_call_propose/,
    );
  });

  test("newCallSid generates a non-empty short identifier per call", () => {
    const a = newCallSid();
    const b = newCallSid();
    expect(a).toMatch(/^c[0-9a-f]+$/);
    expect(a).not.toBe(b);
  });
});
