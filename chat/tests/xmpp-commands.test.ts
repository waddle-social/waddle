import { describe, it, expect, vi } from "vitest";
import { executeCreateChannelCommand } from "@/lib/xmpp/commands";

describe("XMPP Ad-Hoc Commands", () => {
  describe("executeCreateChannelCommand", () => {
    it("should execute create-channel command and return success", async () => {
      const mockSendIQ = vi.fn();
      
      // Mock the execute response (returns data form)
      mockSendIQ.mockResolvedValueOnce({
        command: {
          sid: "test-session-123",
          status: "executing",
          form: {
            type: "form",
            fields: [
              { name: "waddle_id", type: "hidden" },
              { name: "name", type: "text-single" },
              { name: "description", type: "text-multi" },
              { name: "channel_type", type: "list-single" },
              { name: "position", type: "text-single" },
            ],
          },
        },
      });

      // Mock the submit response (returns result)
      mockSendIQ.mockResolvedValueOnce({
        command: {
          sid: "test-session-123",
          status: "completed",
          form: {
            type: "result",
            fields: [
              { name: "channel_id", value: "new-channel-id" },
              { name: "channel_jid", value: "test_waddle_new-channel-id@muc.localhost" },
            ],
          },
          notes: [{ type: "info", value: "Channel created" }],
        },
      });

      const mockXmpp = { sendIQ: mockSendIQ };

      const result = await executeCreateChannelCommand(
        mockXmpp as any,
        "localhost",
        {
          waddleId: "test-waddle",
          name: "Test Channel",
          description: "A test channel",
          channelType: "text",
          position: 0,
        },
      );

      expect(result.success).toBe(true);
      expect(result.channelId).toBe("new-channel-id");
      expect(result.channelJid).toBe("test_waddle_new-channel-id@muc.localhost");
      expect(result.error).toBeUndefined();

      // Verify the command execution flow
      expect(mockSendIQ).toHaveBeenCalledTimes(2);
      
      // First call: execute
      expect(mockSendIQ).toHaveBeenNthCalledWith(1, expect.objectContaining({
        type: "set",
        to: "localhost",
        command: expect.objectContaining({
          node: "waddle:create-channel",
          action: "execute",
        }),
      }));

      // Second call: submit
      expect(mockSendIQ).toHaveBeenNthCalledWith(2, expect.objectContaining({
        type: "set",
        to: "localhost",
        command: expect.objectContaining({
          node: "waddle:create-channel",
          action: "complete",
          sid: "test-session-123",
          form: expect.objectContaining({
            type: "submit",
            fields: expect.arrayContaining([
              expect.objectContaining({ name: "waddle_id", value: "test-waddle" }),
              expect.objectContaining({ name: "name", value: "Test Channel" }),
            ]),
          }),
        }),
      }));
    });

    it("should return error when command fails with error note", async () => {
      const mockSendIQ = vi.fn();
      
      mockSendIQ.mockResolvedValueOnce({
        command: {
          sid: "test-session-456",
          status: "executing",
          form: { type: "form", fields: [] },
        },
      });

      mockSendIQ.mockResolvedValueOnce({
        command: {
          sid: "test-session-456",
          status: "completed",
          notes: [{ type: "error", value: "Permission denied" }],
        },
      });

      const mockXmpp = { sendIQ: mockSendIQ };

      const result = await executeCreateChannelCommand(
        mockXmpp as any,
        "localhost",
        {
          waddleId: "test-waddle",
          name: "Test Channel",
          channelType: "text",
          position: 0,
        },
      );

      expect(result.success).toBe(false);
      expect(result.error).toBe("Permission denied");
      expect(result.channelId).toBeUndefined();
    });

    it("should return error when execute doesn't return session id", async () => {
      const mockSendIQ = vi.fn();
      
      mockSendIQ.mockResolvedValueOnce({
        command: {
          status: "completed",
        },
      });

      const mockXmpp = { sendIQ: mockSendIQ };

      const result = await executeCreateChannelCommand(
        mockXmpp as any,
        "localhost",
        {
          waddleId: "test-waddle",
          name: "Test Channel",
          channelType: "forum",
          position: 1,
        },
      );

      expect(result.success).toBe(false);
      expect(result.error).toBe("Server did not return a session ID");
    });

    it("should handle XMPP errors gracefully", async () => {
      const mockSendIQ = vi.fn();
      
      mockSendIQ.mockRejectedValueOnce({
        error: {
          condition: "forbidden",
          text: "You don't have permission to create channels",
        },
      });

      const mockXmpp = { sendIQ: mockSendIQ };

      const result = await executeCreateChannelCommand(
        mockXmpp as any,
        "localhost",
        {
          waddleId: "test-waddle",
          name: "Test Channel",
          channelType: "text",
          position: 0,
        },
      );

      expect(result.success).toBe(false);
      expect(result.error).toBe("You don't have permission to create channels");
    });

    it("should handle missing channel_id in result", async () => {
      const mockSendIQ = vi.fn();
      
      mockSendIQ.mockResolvedValueOnce({
        command: {
          sid: "test-session-789",
          status: "executing",
          form: { type: "form", fields: [] },
        },
      });

      mockSendIQ.mockResolvedValueOnce({
        command: {
          sid: "test-session-789",
          status: "completed",
          form: {
            type: "result",
            fields: [],
          },
        },
      });

      const mockXmpp = { sendIQ: mockSendIQ };

      const result = await executeCreateChannelCommand(
        mockXmpp as any,
        "localhost",
        {
          waddleId: "test-waddle",
          name: "Test Channel",
          channelType: "text",
          position: 0,
        },
      );

      expect(result.success).toBe(false);
      expect(result.error).toBe("Server did not return channel_id in result form");
    });
  });
});
