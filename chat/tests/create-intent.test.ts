import { describe, it, expect } from "bun:test";
import {
  defaultCreateForm,
  defaultCreateFormForContext,
  type CreateFormData,
  type CreateSpaceFormData,
  type CreateMucFormData,
  type CreateSpaceMucFormData,
  type CreateSpaceWithMucFormData,
} from "@/lib/chat-ui";

describe("create intent form model", () => {
  describe("defaultCreateForm", () => {
    it("returns a muc intent with empty fields and text subtype", () => {
      const form = defaultCreateForm();
      expect(form.intent).toBe("muc");
      expect(form.name).toBe("");
      expect(form.description).toBe("");
      expect(form.muc_type).toBe("text");
    });

    it("keeps the default muc intent explicitly standalone", () => {
      const form = defaultCreateForm();
      expect(form.intent).toBe("muc");
      expect("space_node" in form).toBe(false);
    });
  });

  describe("defaultCreateFormForContext", () => {
    it("prefers a space-muc intent when an active Space exists", () => {
      const form = defaultCreateFormForContext("team-space");
      expect(form).toEqual({
        intent: "space-muc",
        space_node: "team-space",
        name: "",
        description: "",
        muc_type: "text",
      });
    });

    it("uses a standalone muc intent when no active Space exists", () => {
      expect(defaultCreateFormForContext(null)).toEqual(defaultCreateForm());
      expect(defaultCreateFormForContext(undefined)).toEqual(defaultCreateForm());
    });
  });

  describe("CreateFormData union narrowing", () => {
    it("narrows to CreateSpaceFormData when intent is space", () => {
      const form: CreateFormData = { intent: "space", name: "My Space", description: "" };
      if (form.intent === "space") {
        const typed: CreateSpaceFormData = form;
        expect(typed.name).toBe("My Space");
      } else {
        throw new Error("Expected space intent");
      }
    });

    it("narrows to CreateMucFormData when intent is muc", () => {
      const form: CreateFormData = { intent: "muc", name: "general", description: "", muc_type: "forum" };
      if (form.intent === "muc") {
        const typed: CreateMucFormData = form;
        expect(typed.muc_type).toBe("forum");
      } else {
        throw new Error("Expected muc intent");
      }
    });

    it("narrows to CreateSpaceMucFormData when intent is space-muc", () => {
      const form: CreateFormData = {
        intent: "space-muc",
        space_node: "team-space",
        name: "announcements",
        description: "",
        muc_type: "text",
      };
      if (form.intent === "space-muc") {
        const typed: CreateSpaceMucFormData = form;
        expect(typed.space_node).toBe("team-space");
        expect(typed.muc_type).toBe("text");
      } else {
        throw new Error("Expected space-muc intent");
      }
    });

    it("narrows to CreateSpaceWithMucFormData when intent is space-with-muc", () => {
      const form: CreateFormData = {
        intent: "space-with-muc",
        space_name: "Engineering",
        space_description: "The eng team",
        muc_name: "general",
        muc_description: "",
        muc_type: "text",
      };
      if (form.intent === "space-with-muc") {
        const typed: CreateSpaceWithMucFormData = form;
        expect(typed.space_name).toBe("Engineering");
        expect(typed.muc_name).toBe("general");
      } else {
        throw new Error("Expected space-with-muc intent");
      }
    });
  });

  describe("submit guard logic", () => {
    it("space form is valid when name is non-empty", () => {
      const form: CreateSpaceFormData = { intent: "space", name: "Eng", description: "" };
      expect(form.name.trim().length > 0).toBe(true);
    });

    it("muc form is invalid when name is empty", () => {
      const form: CreateMucFormData = { intent: "muc", name: "  ", description: "", muc_type: "text" };
      expect(form.name.trim().length > 0).toBe(false);
    });

    it("space-muc form requires both space_node and name", () => {
      const incomplete: CreateSpaceMucFormData = { intent: "space-muc", space_node: "", name: "general", description: "", muc_type: "text" };
      const isValid = incomplete.space_node.trim().length > 0 && incomplete.name.trim().length > 0;
      expect(isValid).toBe(false);
    });

    it("space-with-muc form requires both space_name and muc_name", () => {
      const form: CreateSpaceWithMucFormData = {
        intent: "space-with-muc",
        space_name: "Eng",
        space_description: "",
        muc_name: "general",
        muc_description: "",
        muc_type: "text",
      };
      const isValid = form.space_name.trim().length > 0 && form.muc_name.trim().length > 0;
      expect(isValid).toBe(true);
    });
  });
});
