import { Construct } from "constructs";
import { VirtualEnvironmentVm } from "../../.gen/providers/proxmox/virtual-environment-vm";

/**
 * Configuration for the TalosVmConstruct.
 */
export interface TalosVmConfig {
  /** Proxmox node name where the VM will be created */
  nodeName: string;
  /** Optional VM ID (auto-assigned if omitted) */
  vmId?: number;
  /** VM display name (e.g., 'talos-cp-01') */
  vmName: string;
  /** Talos image file ID from TalosImageConstruct.fileId */
  imageFileId: string;
  /** CPU configuration */
  cpu: {
    /** Number of CPU cores */
    cores: number;
    /** CPU type (default: 'x86-64-v2-AES' for Talos compatibility) */
    type?: string;
  };
  /** Memory configuration */
  memory: {
    /** Dedicated memory in MB */
    dedicated: number;
  };
  /** Disk configuration */
  disk: {
    /** Proxmox storage ID for the disk */
    datastoreId: string;
    /** Disk size in GB */
    size: number;
    /** Disk interface (default: 'scsi0') */
    interface?: string;
  };
  /** Network configuration */
  network: {
    /** Network bridge name (e.g., 'vmbr0') */
    bridge: string;
    /** Optional static IPv4 configuration (omit for DHCP) */
    ipv4?: {
      /** IP address without CIDR suffix (e.g., '192.168.1.101') */
      address: string;
      /** Gateway address (e.g., '192.168.1.1') */
      gateway: string;
      /** Network mask as CIDR suffix (default: '24') */
      netmask?: string;
    };
  };
  /** Optional Proxmox VM tags for organization */
  tags?: string[];
  /** Kubernetes node labels (stored in VM description, applied in Phase 4) */
  nodeLabels?: Record<string, string>;
  /** Optional VM description */
  description?: string;
}

/**
 * Static IP network configuration for Talos nodes.
 */
export interface TalosStaticNetworkConfig {
  /** IP address prefix (first 3 octets for /24 networks, e.g., '192.168.1') */
  ipPrefix: string;
  /** Starting IP suffix (last octet, e.g., 101) */
  ipStart: number;
  /** Network gateway IP */
  gateway: string;
  /** Network CIDR suffix (default: '24') */
  netmask: string;
}

/**
 * Network configuration for Talos cluster.
 * When 'static' is provided, nodes receive sequential static IPs.
 * When 'static' is omitted, nodes use DHCP.
 */
export interface TalosNetworkConfig {
  /** Static IP configuration (omit for DHCP mode) */
  static?: TalosStaticNetworkConfig;
}

/**
 * Worker node specifications for Talos cluster.
 */
export interface TalosWorkerConfig {
  /** Number of worker nodes */
  count: number;
  /** CPU cores per worker */
  cores: number;
  /** Memory in MB per worker */
  memory: number;
  /** Disk size in GB per worker */
  diskSize: number;
}

/**
 * Talos cluster configuration for ProxmoxStack integration.
 */
export interface TalosClusterConfig {
  /** Proxmox node name */
  nodeName: string;
  /** Storage ID for VM disks (may be Ceph pool, local-lvm, etc.) */
  storageId: string;
  /** Storage ID for ISO/image content (must support 'iso' content type, default: 'local') */
  imageStorageId: string;
  /** Network bridge for VMs */
  networkBridge: string;
  /** Talos version to deploy */
  talosVersion: string;
  /** Kubernetes cluster name */
  clusterName: string;
  /** Cluster API endpoint URL */
  clusterEndpoint: string;
  /** Control plane specifications */
  controlPlane: {
    count: number;
    cores: number;
    memory: number;
    diskSize: number;
  };
  /** Worker node specifications (optional, omit for control-plane-only cluster) */
  workers?: TalosWorkerConfig;
  /** Network configuration (static IPs or DHCP) */
  network: TalosNetworkConfig;
  /** Topology labels */
  topology: {
    region: string;
    zone: string;
  };
  /** Optional Kubernetes version (uses Talos default if not specified) */
  kubernetesVersion?: string;
  /** Cluster domain for Kubernetes services (default: 'cluster.local') */
  clusterDomain?: string;
  /** Pod network CIDR (default: '10.244.0.0/16') */
  clusterNetwork?: string;
  /** Service network CIDR (default: '10.96.0.0/12') */
  serviceNetwork?: string;
  /** CNI name ('none' for external CNI like Cilium, 'flannel' for built-in) */
  cni?: string;
  /** Installation disk path (default: '/dev/sda') */
  installDisk?: string;
  /** Allow scheduling workloads on control plane nodes (auto-determined if not set) */
  allowSchedulingOnControlPlanes?: boolean;
}

/**
 * A construct for provisioning Talos VMs on Proxmox.
 *
 * Creates a single Talos VM with specified resources and network configuration.
 * VMs are configured with settings optimized for Talos Linux:
 *
 * - Memory ballooning disabled (Talos doesn't support qemu-guest-agent)
 * - Serial device enabled (required for Talos console access)
 * - QEMU agent disabled (not supported by Talos)
 * - CPU type x86-64-v2-AES (minimum requirement for Talos)
 * - VirtIO network adapter (best performance)
 * - SCSI disk with iothread (best performance)
 *
 * Node labels are stored in the VM description as JSON and will be applied
 * to Kubernetes nodes during Phase 4 bootstrap via Talos machine config.
 *
 * @see https://www.talos.dev/v1.11/talos-guides/install/virtualized-platforms/proxmox/
 */
export class TalosVmConstruct extends Construct {
  /** The underlying VM resource */
  public readonly vm: VirtualEnvironmentVm;
  /** Computed VM ID */
  public readonly vmId: number | undefined;
  /** Configured IP address (from config or DHCP) */
  public readonly ipAddress: string | undefined;
  /** Kubernetes node labels for reference */
  public readonly nodeLabels: Record<string, string>;

  constructor(scope: Construct, id: string, config: TalosVmConfig) {
    super(scope, id);

    this.vmId = config.vmId;
    this.ipAddress = config.network.ipv4?.address;
    this.nodeLabels = config.nodeLabels ?? {};

    // Build description with node labels as JSON for reference
    const labelsJson = Object.keys(this.nodeLabels).length > 0
      ? `\n\nNode Labels:\n${JSON.stringify(this.nodeLabels, null, 2)}`
      : "";
    const description = `${config.description ?? "Talos Linux VM"}${labelsJson}`;

    // Combine tags
    const tags = [...(config.tags ?? []), "talos", "kubernetes"];
    const uniqueTags = [...new Set(tags)];

    // Build initialization config for static IP or DHCP
    const ipConfig = config.network.ipv4
      ? {
          ipv4: {
            address: `${config.network.ipv4.address}/${config.network.ipv4.netmask ?? "24"}`,
            gateway: config.network.ipv4.gateway,
          },
        }
      : undefined;

    this.vm = new VirtualEnvironmentVm(this, "vm", {
      name: config.vmName,
      nodeName: config.nodeName,
      vmId: config.vmId,
      description: description,
      tags: uniqueTags.sort(),
      started: true,
      onBoot: true,
      stopOnDestroy: true,

      cpu: {
        cores: config.cpu.cores,
        type: config.cpu.type ?? "x86-64-v2-AES",
        // architecture: "x86_64", // Removed to avoid permission errors with API token
      },

      memory: {
        dedicated: config.memory.dedicated,
        floating: 0, // Disable ballooning - Talos doesn't support it
      },

      // Use virtio-scsi-single controller for iothread support
      scsiHardware: "virtio-scsi-single",

      // VGA display for console access (Talos outputs to both serial and VGA)
      vga: {
        type: "std",
      },

      disk: [
        {
          datastoreId: config.disk.datastoreId,
          fileId: config.imageFileId,
          interface: config.disk.interface ?? "scsi0",
          size: config.disk.size,
          discard: "on",
          iothread: true,
          ssd: true,
        },
      ],

      networkDevice: [
        {
          bridge: config.network.bridge,
          model: "virtio",
        },
      ],

      serialDevice: [
        {
          device: "socket",
        },
      ],

      initialization: {
        datastoreId: config.disk.datastoreId,
        type: "nocloud",
        ipConfig: ipConfig ? [ipConfig] : undefined,
      },

      agent: {
        enabled: false, // Talos doesn't use qemu-guest-agent
      },
    });
  }

  /**
   * Adds an explicit dependency on another construct.
   * Use this to ensure the image is downloaded before VM creation.
   */
  addDependency(construct: Construct): void {
    this.vm.node.addDependency(construct);
  }
}
