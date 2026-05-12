import { describe, expect, test } from "bun:test";
import { applyDeliveryEvent } from "../src/lib/xmpp/delivery-lifecycle";

// XEP-0184 received and XEP-0198 stream-management acks both feed the
// delivery lifecycle. The pure helper enforces the "delivered is terminal,
// never downgrade" invariant and keeps every transition idempotent — a
// duplicate ack/queue-status/failure event after a confirmed delivery is a
// no-op, never a regression.

describe("applyDeliveryEvent — forward transitions", () => {
  test("undefined → queued (initial outbound)", () => {
    expect(applyDeliveryEvent(undefined, "queued")).toBe("queued");
  });

  test("queued → sending (transport picks up the stanza)", () => {
    expect(applyDeliveryEvent("queued", "sending")).toBe("sending");
  });

  test("sending → delivered (XEP-0184 receipt / XEP-0198 ack / self-echo)", () => {
    expect(applyDeliveryEvent("sending", "delivered")).toBe("delivered");
  });

  test("sending → failed (transport gave up)", () => {
    expect(applyDeliveryEvent("sending", "failed")).toBe("failed");
  });

  test("queued → failed (queue rejection)", () => {
    expect(applyDeliveryEvent("queued", "failed")).toBe("failed");
  });

  test("failed → sending (retry kicks in)", () => {
    expect(applyDeliveryEvent("failed", "sending")).toBe("sending");
  });

  test("failed → delivered (delayed ack arrives after failure timeout)", () => {
    // A late XEP-0184 receipt or XEP-0198 ack proves the stanza got through
    // even though we'd given up on it.
    expect(applyDeliveryEvent("failed", "delivered")).toBe("delivered");
  });
});

describe("applyDeliveryEvent — terminal 'delivered' (no downgrade)", () => {
  test("delivered + queued → delivered (duplicate XEP-0198 retransmit)", () => {
    expect(applyDeliveryEvent("delivered", "queued")).toBe("delivered");
  });

  test("delivered + sending → delivered (late retry signal)", () => {
    expect(applyDeliveryEvent("delivered", "sending")).toBe("delivered");
  });

  test("delivered + failed → delivered (stale failure signal after confirm)", () => {
    expect(applyDeliveryEvent("delivered", "failed")).toBe("delivered");
  });

  test("delivered + delivered → delivered (idempotent ack)", () => {
    expect(applyDeliveryEvent("delivered", "delivered")).toBe("delivered");
  });
});

describe("applyDeliveryEvent — undefined current state", () => {
  test("undefined + sending → sending (non-canonical but accepted)", () => {
    expect(applyDeliveryEvent(undefined, "sending")).toBe("sending");
  });

  test("undefined + delivered → delivered", () => {
    expect(applyDeliveryEvent(undefined, "delivered")).toBe("delivered");
  });

  test("undefined + failed → failed", () => {
    expect(applyDeliveryEvent(undefined, "failed")).toBe("failed");
  });
});
