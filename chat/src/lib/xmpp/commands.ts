/** XEP-0050: Ad-Hoc Commands execution for Waddle operations. */
import type { Agent } from "stanza";
import type { IQ, AdHocCommand, DataForm } from "stanza/protocol";

interface CreateChannelCommandInput {
  name: string;
  description?: string;
  channelType: "text" | "forum";
  position: number;
}

interface CreateChannelCommandResult {
  success: boolean;
  channelId?: string;
  channelJid?: string;
  error?: string;
}

interface CommandResponse {
  sid?: string;
  status?: string;
  form?: DataForm;
  notes?: Array<{ type?: string; value?: string }>;
}

function parseCommandResponse(iq: IQ): CommandResponse {
  const command = iq.command;
  if (!command) return {};

  return {
    sid: command.sid,
    status: command.status,
    form: command.form,
    notes: command.notes,
  };
}

/**
 * Execute the `waddle:create-channel` ad-hoc command.
 * 
 * Flow:
 * 1. Send execute action to initiate the command
 * 2. Parse the returned data form and session ID
 * 3. Submit the filled form with user inputs
 * 4. Parse the result for success/failure
 * 
 * @param xmpp - XMPP client agent
 * @param serverJid - Server JID to send the command to (e.g., "localhost")
 * @param input - Channel creation inputs
 * @returns Result with success status, channel ID/JID, or error message
 */
export async function executeCreateChannelCommand(
  xmpp: Agent,
  serverJid: string,
  input: CreateChannelCommandInput,
): Promise<CreateChannelCommandResult> {
  try {
    // Step 1: Execute the command to get the data form
    const executeIq = await xmpp.sendIQ({
      type: "set",
      to: serverJid,
      command: {
        type: "command",
        node: "waddle:create-channel",
        action: "execute",
      } as AdHocCommand,
    } as IQ);

    const executeResponse = parseCommandResponse(executeIq);

    if (!executeResponse.sid) {
      return {
        success: false,
        error: "Server did not return a session ID",
      };
    }

    // Step 2: Fill and submit the data form
    const submitIq = await xmpp.sendIQ({
      type: "set",
      to: serverJid,
      command: {
        type: "command",
        node: "waddle:create-channel",
        action: "complete",
        sid: executeResponse.sid,
        form: {
          type: "submit",
          fields: [
            { name: "name", value: input.name, type: "text-single" },
            ...(input.description
              ? [{ name: "description", value: input.description, type: "text-multi" }]
              : []),
            { name: "channel_type", value: input.channelType, type: "list-single" },
            { name: "position", value: String(input.position), type: "text-single" },
          ],
        },
      } as AdHocCommand,
    } as IQ);

    const submitResponse = parseCommandResponse(submitIq);

    // Step 3: Parse the result
    if (submitResponse.status === "completed") {
      // Look for error notes first
      const errorNote = submitResponse.notes?.find((n) => n.type === "error");
      if (errorNote) {
        return {
          success: false,
          error: errorNote.value ?? "Command failed",
        };
      }

      // Parse result form for channel_id and channel_jid
      const resultFields = submitResponse.form?.fields ?? [];
      const channelIdField = resultFields.find((f) => f.name === "channel_id");
      const channelJidField = resultFields.find((f) => f.name === "channel_jid");

      const channelId = Array.isArray(channelIdField?.value)
        ? channelIdField.value[0]
        : channelIdField?.value;
      const channelJid = Array.isArray(channelJidField?.value)
        ? channelJidField.value[0]
        : channelJidField?.value;

      if (!channelId) {
        return {
          success: false,
          error: "Server did not return channel_id in result form",
        };
      }

      return {
        success: true,
        channelId: String(channelId),
        channelJid: channelJid ? String(channelJid) : undefined,
      };
    }

    // Command not completed
    const firstNote = submitResponse.notes?.[0];
    return {
      success: false,
      error: firstNote?.value ?? `Command ended with status: ${submitResponse.status ?? "unknown"}`,
    };
  } catch (error: unknown) {
    // Handle XMPP errors
    const errorObj = error as { error?: { condition?: string; text?: string } };
    const condition = errorObj?.error?.condition;
    const text = errorObj?.error?.text;

    let errorMessage = "Failed to execute create-channel command";
    if (text) {
      errorMessage = text;
    } else if (condition) {
      errorMessage = `Command error: ${condition}`;
    }

    return {
      success: false,
      error: errorMessage,
    };
  }
}
