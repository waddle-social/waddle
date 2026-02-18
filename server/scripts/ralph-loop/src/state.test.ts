import { describe, test, expect } from "bun:test";
import { createActor } from "xstate";
import { ralphMachine, canTransition } from "./state.js";

describe("ralph state machine", () => {
  describe("PLAN state transitions", () => {
    test("PLAN → BUILD via START_BUILD", () => {
      const actor = createActor(ralphMachine);
      actor.start();

      expect(actor.getSnapshot().value).toBe("PLAN");
      actor.send({ type: "START_BUILD", reason: "Plan approved" });
      expect(actor.getSnapshot().value).toBe("BUILD");

      actor.stop();
    });

    test("PLAN → REVIEW via SKIP_TO_REVIEW", () => {
      const actor = createActor(ralphMachine);
      actor.start();

      actor.send({ type: "SKIP_TO_REVIEW", reason: "Code already exists, review it" });
      expect(actor.getSnapshot().value).toBe("REVIEW");

      actor.stop();
    });

    test("PLAN → END via NOTHING_TO_DO", () => {
      const actor = createActor(ralphMachine);
      actor.start();

      actor.send({ type: "NOTHING_TO_DO", reason: "All tasks complete" });
      expect(actor.getSnapshot().value).toBe("END");
      expect(actor.getSnapshot().status).toBe("done");

      actor.stop();
    });

    test("PLAN rejects invalid events", () => {
      expect(canTransition("PLAN", "APPROVED")).toBe(false);
      expect(canTransition("PLAN", "NEEDS_FIXES")).toBe(false);
      expect(canTransition("PLAN", "IMPLEMENTATION_DONE")).toBe(false);
    });
  });

  describe("BUILD state transitions", () => {
    test("BUILD → REVIEW via IMPLEMENTATION_DONE", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });

      expect(actor.getSnapshot().value).toBe("BUILD");
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "Implementation complete" });
      expect(actor.getSnapshot().value).toBe("REVIEW");

      actor.stop();
    });

    test("BUILD → PLAN via BLOCKED", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });

      actor.send({ type: "BLOCKED", reason: "Missing dependency" });
      expect(actor.getSnapshot().value).toBe("PLAN");

      actor.stop();
    });

    test("BUILD → BUILD via CONTINUE_BUILDING (self-transition)", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });

      const historyBefore = actor.getSnapshot().context.history.length;
      actor.send({ type: "CONTINUE_BUILDING", reason: "More work needed" });
      expect(actor.getSnapshot().value).toBe("BUILD");
      expect(actor.getSnapshot().context.history.length).toBe(historyBefore + 1);

      actor.stop();
    });

    test("BUILD rejects invalid events", () => {
      expect(canTransition("BUILD", "APPROVED")).toBe(false);
      expect(canTransition("BUILD", "START_BUILD")).toBe(false);
      expect(canTransition("BUILD", "NOTHING_TO_DO")).toBe(false);
    });
  });

  describe("REVIEW state transitions", () => {
    test("REVIEW → END via APPROVED", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "done" });

      expect(actor.getSnapshot().value).toBe("REVIEW");
      actor.send({ type: "APPROVED", reason: "All criteria met" });
      expect(actor.getSnapshot().value).toBe("END");
      expect(actor.getSnapshot().status).toBe("done");

      actor.stop();
    });

    test("REVIEW → BUILD via NEEDS_FIXES", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "done" });

      actor.send({ type: "NEEDS_FIXES", reason: "Test failures" });
      expect(actor.getSnapshot().value).toBe("BUILD");

      actor.stop();
    });

    test("REVIEW → PLAN via REJECTED", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "done" });

      actor.send({ type: "REJECTED", reason: "Wrong approach" });
      expect(actor.getSnapshot().value).toBe("PLAN");

      actor.stop();
    });

    test("REVIEW → PLAN via NEXT_ITERATION increments iteration", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "done" });

      expect(actor.getSnapshot().context.iteration).toBe(1);
      actor.send({ type: "NEXT_ITERATION", reason: "Ready for next task" });
      expect(actor.getSnapshot().value).toBe("PLAN");
      expect(actor.getSnapshot().context.iteration).toBe(2);

      actor.stop();
    });

    test("REVIEW → REVIEW via MORE_REVIEW (self-transition)", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "START_BUILD", reason: "start" });
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "done" });

      const historyBefore = actor.getSnapshot().context.history.length;
      actor.send({ type: "MORE_REVIEW", reason: "Additional review needed" });
      expect(actor.getSnapshot().value).toBe("REVIEW");
      expect(actor.getSnapshot().context.history.length).toBe(historyBefore + 1);

      actor.stop();
    });

    test("REVIEW rejects invalid events", () => {
      expect(canTransition("REVIEW", "START_BUILD")).toBe(false);
      expect(canTransition("REVIEW", "NOTHING_TO_DO")).toBe(false);
      expect(canTransition("REVIEW", "BLOCKED")).toBe(false);
    });
  });

  describe("END state", () => {
    test("END is a final state", () => {
      const actor = createActor(ralphMachine);
      actor.start();
      actor.send({ type: "NOTHING_TO_DO", reason: "done" });

      expect(actor.getSnapshot().status).toBe("done");
      expect(canTransition("END", "START_BUILD")).toBe(false);
      expect(canTransition("END", "APPROVED")).toBe(false);

      actor.stop();
    });
  });

  describe("history recording", () => {
    test("records transitions in history", () => {
      const actor = createActor(ralphMachine);
      actor.start();

      actor.send({ type: "START_BUILD", reason: "Plan approved" });
      actor.send({ type: "IMPLEMENTATION_DONE", reason: "Code written" });
      actor.send({ type: "APPROVED", reason: "Tests pass" });

      const history = actor.getSnapshot().context.history;
      expect(history.length).toBe(3);

      expect(history[0].phase).toBe("PLAN");
      expect(history[0].transition).toBe("BUILD");
      expect(history[0].reason).toBe("Plan approved");

      expect(history[1].phase).toBe("BUILD");
      expect(history[1].transition).toBe("REVIEW");
      expect(history[1].reason).toBe("Code written");

      expect(history[2].phase).toBe("REVIEW");
      expect(history[2].transition).toBe("END");
      expect(history[2].reason).toBe("Tests pass");

      actor.stop();
    });
  });

  describe("canTransition helper", () => {
    test("returns true for valid transitions", () => {
      expect(canTransition("PLAN", "START_BUILD")).toBe(true);
      expect(canTransition("PLAN", "NOTHING_TO_DO")).toBe(true);
      expect(canTransition("BUILD", "IMPLEMENTATION_DONE")).toBe(true);
      expect(canTransition("REVIEW", "APPROVED")).toBe(true);
    });

    test("returns false for invalid transitions", () => {
      expect(canTransition("PLAN", "APPROVED")).toBe(false);
      expect(canTransition("BUILD", "NOTHING_TO_DO")).toBe(false);
      expect(canTransition("REVIEW", "START_BUILD")).toBe(false);
      expect(canTransition("END", "START_BUILD")).toBe(false);
    });
  });
});
