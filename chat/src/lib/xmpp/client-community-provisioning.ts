import type { PinPermission } from "@/lib/chat-types";
import {
  configureMucRoom as configureMucRoomWithProtocol,
  createMucInSpace as createMucInSpaceWithProtocol,
  createMucRoom as createMucRoomWithProtocol,
  createSpaceNode as createSpaceNodeWithProtocol,
  createSpaceWithMuc as createSpaceWithMucWithProtocol,
  moveMucToSpace as moveMucToSpaceWithProtocol,
} from "./protocol-helpers";

type MucType = "text" | "forum";

interface CommunityProvisioningTransport {
  send_raw_iq?: (xml: string) => Promise<string>;
  join_room?: (roomJid: string, nick: string) => Promise<void>;
  leave_room?: (roomJid: string, nick: string) => Promise<void>;
}

interface CommunityProvisioningDeps {
  requireConnectedXmpp: () => Promise<CommunityProvisioningTransport>;
  nick: () => string;
}

interface ConfigureMucRoomRequest {
  roomJid: string;
  name: string;
  description?: string;
  pinPermission?: PinPermission;
}

interface CreateMucRoomRequest {
  mucServiceJid: string;
  roomLocalpart: string;
  name: string;
  description?: string;
  mucType?: MucType;
}

interface CreateMucRoomResult {
  roomJid: string;
}

interface CreateSpaceNodeRequest {
  spacesServiceJid: string;
  nodeId?: string;
  name: string;
  description?: string;
}

interface CreateSpaceNodeResult {
  node: string;
  serviceJid: string;
}

interface CreateMucInSpaceRequest extends CreateMucRoomRequest {
  spacesServiceJid: string;
  spaceNode: string;
}

interface CreateMucInSpaceResult {
  roomJid: string;
  spaceNode: string;
  spacesServiceJid: string;
}

interface CreateSpaceWithMucRequest {
  mucServiceJid: string;
  spacesServiceJid: string;
  spaceNodeId?: string;
  spaceName: string;
  spaceDescription?: string;
  roomLocalpart: string;
  mucName: string;
  mucDescription?: string;
  mucType?: MucType;
}

interface CreateSpaceWithMucResult {
  roomJid: string;
  spaceNode: string;
  spacesServiceJid: string;
}

interface MoveMucToSpaceRequest {
  spacesServiceJid: string;
  targetSpaceNode: string;
  mucJid: string;
  name: string;
  autojoin?: boolean;
}

export class CommunityProvisioning {
  constructor(private readonly deps: CommunityProvisioningDeps) {}

  async configureMucRoom(request: ConfigureMucRoomRequest): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await configureMucRoomWithProtocol(xmpp, request.roomJid, {
      name: request.name,
      description: request.description,
      pinPermission: request.pinPermission,
    });
  }

  async createMucRoom(request: CreateMucRoomRequest): Promise<CreateMucRoomResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    return createMucRoomWithProtocol(xmpp, request.mucServiceJid, {
      roomLocalpart: request.roomLocalpart,
      nick: this.deps.nick(),
      name: request.name,
      description: request.description,
      mucType: request.mucType,
    });
  }

  async createSpaceNode(request: CreateSpaceNodeRequest): Promise<CreateSpaceNodeResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    return createSpaceNodeWithProtocol(xmpp, request.spacesServiceJid, {
      nodeId: request.nodeId,
      name: request.name,
      description: request.description,
    });
  }

  async createMucInSpace(request: CreateMucInSpaceRequest): Promise<CreateMucInSpaceResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    return createMucInSpaceWithProtocol(
      xmpp,
      request.mucServiceJid,
      request.spacesServiceJid,
      {
        roomLocalpart: request.roomLocalpart,
        nick: this.deps.nick(),
        name: request.name,
        description: request.description,
        mucType: request.mucType,
        spaceNode: request.spaceNode,
      },
    );
  }

  async createSpaceWithMuc(request: CreateSpaceWithMucRequest): Promise<CreateSpaceWithMucResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    return createSpaceWithMucWithProtocol(
      xmpp,
      request.mucServiceJid,
      request.spacesServiceJid,
      {
        spaceNodeId: request.spaceNodeId,
        spaceName: request.spaceName,
        spaceDescription: request.spaceDescription,
        roomLocalpart: request.roomLocalpart,
        nick: this.deps.nick(),
        mucName: request.mucName,
        mucDescription: request.mucDescription,
        mucType: request.mucType,
      },
    );
  }

  async moveMucToSpace(request: MoveMucToSpaceRequest): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await moveMucToSpaceWithProtocol(
      xmpp,
      request.spacesServiceJid,
      request.targetSpaceNode,
      request.mucJid,
      {
        name: request.name,
        autojoin: request.autojoin,
      },
    );
  }
}
