import { Construct } from "constructs";
import { VirtualEnvironmentVm } from "../../.gen/providers/proxmox/virtual-environment-vm";
import { VirtualEnvironmentDownloadFile } from "../../.gen/providers/proxmox/virtual-environment-download-file";

/**
 * Configuration for the TeleportVmConstruct.
 *
 * Note: The Teleport-specific fields (teleportDomain, letsencryptEmail, teleportVersion)
 * are captured for informational purposes only. They are included in the VM description
 * and exposed as construct properties for operator reference. Teleport installation
 * must be performed manually; see docs/teleport-setup.md.
 */
export interface TeleportVmConfig {
  /** Proxmox node name where the VM will be created */
  nodeName: string;
  /** VM display name (default: 'teleport') */
  vmName?: string;
  /** Optional VM ID (auto-assigned if omitted) */
  vmId?: number;
  /** Proxmox storage ID for VM disk (e.g., 'local-lvm') */
  storageId: string;
  /** Proxmox storage ID for cloud images (must support 'iso' content type, e.g., 'local') */
  imageStorageId: string;
  /** Network bridge name (e.g., 'vmbr0') */
  networkBridge: string;
  /** CPU configuration */
  cpu?: {
    /** Number of CPU cores (default: 2, minimum: 2) */
    cores?: number;
    /** CPU type (default: 'x86-64-v2-AES') */
    type?: string;
  };
  /** Memory in MB (default: 4096, minimum: 2048) */
  memory?: number;
  /** Disk size in GB (default: 50, minimum: 20) */
  diskSize?: number;
  /** Network configuration */
  network: {
    /** Static IPv4 configuration */
    ipv4: {
      /** IP address without CIDR suffix (e.g., '192.168.1.100') */
      address: string;
      /** Gateway address (e.g., '192.168.1.1') */
      gateway: string;
      /** Network mask as CIDR suffix (default: '24') */
      netmask?: string;
    };
  };
  /** SSH public keys for initial access (array of public key strings) */
  sshKeys: string[];
  /** Public domain for Teleport - informational only, used in outputs/description */
  teleportDomain: string;
  /** Email for Let's Encrypt - informational only, used in outputs/description */
  letsencryptEmail: string;
  /** Teleport version - informational only, used in outputs/description (default: 'latest') */
  teleportVersion?: string;
  /** Optional Proxmox VM tags for organization */
  tags?: string[];
  /** Deployment environment (e.g., 'production', 'staging') */
  environment?: string;
}

/**
 * A construct for provisioning a base VM on Proxmox for Teleport.
 *
 * This construct provisions a Debian 12 VM with SSH access and static IP only.
 * Teleport software must be installed manually after VM deployment.
 *
 * **What this construct provisions:**
 * - Debian 12 (Bookworm) cloud image
 * - Static IP and SSH key access via cloud-init
 * - QEMU guest agent enabled for Proxmox integration
 *
 * **What this construct does NOT do:**
 * - Install Teleport software
 * - Configure /etc/teleport.yaml
 * - Set up firewall rules
 * - Obtain Let's Encrypt certificates
 *
 * The configuration fields (teleportDomain, letsencryptEmail, teleportVersion)
 * are captured as informational outputs and included in the VM description
 * for operator reference during manual installation. They are not used to
 * configure the VM itself.
 *
 * **After deployment:**
 * Follow the manual installation steps in `docs/teleport-setup.md` to:
 * 1. SSH to the VM: `ssh admin@{ip-address}`
 * 2. Install Teleport from official APT repository
 * 3. Configure /etc/teleport.yaml with your domain and ACME settings
 * 4. Configure firewall and start the Teleport service
 * 5. Create the first admin user
 *
 * @see docs/teleport-setup.md for complete installation instructions
 * @see https://goteleport.com/docs/
 */
export class TeleportVmConstruct extends Construct {
  /** The underlying VM resource */
  public readonly vm: VirtualEnvironmentVm;
  /** The cloud image download resource */
  public readonly imageResource: VirtualEnvironmentDownloadFile;
  /** Computed VM ID */
  public readonly vmId: number | undefined;
  /** Configured IP address */
  public readonly ipAddress: string;
  /** Teleport public domain */
  public readonly teleportDomain: string;
  /** File ID for the cloud image */
  public readonly imageFileId: string;
  /** Let's Encrypt email for ACME */
  public readonly letsencryptEmail: string;
  /** Teleport version */
  public readonly teleportVersion: string;

  constructor(scope: Construct, id: string, config: TeleportVmConfig) {
    super(scope, id);

    this.vmId = config.vmId;
    this.ipAddress = config.network.ipv4.address;
    this.teleportDomain = config.teleportDomain;
    this.letsencryptEmail = config.letsencryptEmail;
    this.teleportVersion = config.teleportVersion ?? "latest";

    const cores = config.cpu?.cores ?? 2;
    const memory = config.memory ?? 4096;
    const diskSize = config.diskSize ?? 50;
    const vmName = config.vmName ?? "teleport";
    const netmask = config.network.ipv4.netmask ?? "24";

    // Download Debian 12 cloud image
    // NOTE: Using .img extension instead of .qcow2 because Proxmox 'iso' content type 
    // strictly expects .iso or .img extensions in some versions, even if content is qcow2.
    const imageFileName = "debian-12-generic-amd64.img";
    const imageUrl = "https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2";

    this.imageResource = new VirtualEnvironmentDownloadFile(this, "image", {
      contentType: "iso",
      datastoreId: config.imageStorageId,
      nodeName: config.nodeName,
      url: imageUrl,
      fileName: imageFileName,
      overwrite: false,
    });

    this.imageFileId = `${config.imageStorageId}:iso/${imageFileName}`;

    // Build description with setup instructions
    const description = [
      "Teleport Auth + Proxy Server",
      `Domain: ${config.teleportDomain}`,
      `Version: ${this.teleportVersion}`,
      `ACME Email: ${config.letsencryptEmail}`,
      "",
      "Provides zero-trust access to:",
      "- Proxmox web UI (Application Access)",
      "- Proxmox SSH (SSH Access)",
      "- Kubernetes API (Kube Agent - Phase 6+)",
      "",
      "Ports to expose:",
      "- 443: HTTPS/Proxy",
      "- 3024: SSH Proxy",
      "",
      "Setup: See docs/teleport-setup.md",
    ].join("\n");

    // Combine tags
    const baseTags = ["teleport", "security"];
    if (config.environment) {
      baseTags.push(config.environment);
    }
    const tags = [...baseTags, ...(config.tags ?? [])];
    const uniqueTags = [...new Set(tags)];

    // Build IP configuration
    const ipConfig = {
      ipv4: {
        address: `${config.network.ipv4.address}/${netmask}`,
        gateway: config.network.ipv4.gateway,
      },
    };

    this.vm = new VirtualEnvironmentVm(this, "vm", {
      name: vmName,
      nodeName: config.nodeName,
      vmId: config.vmId,
      description: description,
      tags: uniqueTags.sort(),
      started: true,
      onBoot: true,
      stopOnDestroy: true,

      cpu: {
        cores: cores,
        type: config.cpu?.type ?? "x86-64-v2-AES",
        // architecture: "x86_64", // Removed to avoid permission errors with API token
      },

      memory: {
        dedicated: memory,
        floating: 0,
      },

      // Use virtio-scsi-single controller for iothread support
      scsiHardware: "virtio-scsi-single",

      disk: [
        {
          datastoreId: config.storageId,
          fileId: this.imageFileId,
          interface: "scsi0",
          size: diskSize,
          discard: "on",
          iothread: true,
          ssd: true,
        },
      ],

      networkDevice: [
        {
          bridge: config.networkBridge,
          model: "virtio",
        },
      ],

      serialDevice: [
        {
          device: "socket",
        },
      ],

      initialization: {
        datastoreId: config.storageId,
        type: "nocloud",
        ipConfig: [ipConfig],
        userAccount: {
          username: "admin",
          keys: config.sshKeys,
        },
      },

      agent: {
        enabled: true,
      },
    });

    // Ensure image is downloaded before VM creation
    this.vm.node.addDependency(this.imageResource);
  }

  /**
   * Adds an explicit dependency on another construct.
   */
  addDependency(construct: Construct): void {
    this.vm.node.addDependency(construct);
  }
}
