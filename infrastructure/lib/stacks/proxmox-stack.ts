import { Construct } from "constructs";
import { TerraformStack, TerraformVariable, TerraformOutput } from "cdktf";
import {
  ProxmoxProviderConfig,
  ProxmoxProviderConstruct,
  TalosProviderConfig,
  TalosProviderConstruct,
} from "../providers";
import {
  TalosImageConstruct,
  TalosVmConstruct,
  TalosClusterConfig,
  TalosClusterBootstrapConstruct,
  TeleportVmConstruct,
} from "../constructs";

/**
 * Configuration for the Teleport VM.
 *
 * Note: This construct provisions a base Debian VM with SSH access and static IP only.
 * Teleport software installation is out of scope and must be performed manually.
 * See docs/teleport-setup.md for installation instructions.
 *
 * The Teleport-specific fields (teleportDomain, letsencryptEmail, teleportVersion)
 * are surfaced as Terraform outputs and included in the VM description for
 * operator reference during manual installation. They do not configure the VM itself.
 */
export interface TeleportConfig {
  /** Enable Teleport VM provisioning */
  enabled: boolean;
  /** Proxmox node name where the VM will be created */
  nodeName: string;
  /** VM display name (default: 'teleport') */
  vmName?: string;
  /** Proxmox storage ID for VM disk (e.g., 'local-lvm') */
  storageId: string;
  /** Proxmox storage ID for cloud images (must support 'iso' content type, e.g., 'local') */
  imageStorageId: string;
  /** Network bridge name (e.g., 'vmbr0') */
  networkBridge: string;
  /** Static IP address for the Teleport VM */
  ipAddress: string;
  /** Network gateway */
  gateway: string;
  /** Network CIDR suffix (default: '24') */
  netmask?: string;
  /** Public domain for Teleport - informational only, used in outputs and VM description */
  teleportDomain: string;
  /** Email for Let's Encrypt - informational only, used in outputs and VM description */
  letsencryptEmail: string;
  /** SSH public keys for initial access */
  sshKeys: string[];
  /** CPU cores (default: 2) */
  cores?: number;
  /** Memory in MB (default: 4096) */
  memory?: number;
  /** Disk size in GB (default: 50) */
  diskSize?: number;
  /** Teleport version - informational only, used in outputs and VM description */
  teleportVersion?: string;
}

/**
 * Configuration for the ProxmoxStack.
 */
export interface ProxmoxStackConfig {
  /**
   * Proxmox provider configuration.
   * If not provided, values will be sourced from Terraform variables.
   */
  proxmoxConfig?: Partial<ProxmoxProviderConfig>;
  /** Optional Talos provider configuration */
  talosConfig?: TalosProviderConfig;
  /** Deployment environment (e.g., 'production', 'staging', 'development') */
  environment: string;
  /**
   * Optional Talos cluster configuration for VM provisioning.
   * If provided, control plane VMs will be created on Proxmox.
   */
  talosCluster?: TalosClusterConfig;
  /**
   * Optional Teleport configuration for secure access.
   * If enabled, a Teleport VM will be provisioned for zero-trust access
   * to Proxmox and Kubernetes infrastructure.
   */
  teleportConfig?: TeleportConfig;
}

export { TalosClusterConfig };

/**
 * The primary stack for provisioning Talos Kubernetes on Proxmox.
 *
 * This stack serves as the foundation for:
 * - Provisioning Talos VMs on Proxmox (Phase 3)
 * - Bootstrapping the Kubernetes cluster (Phase 4)
 * - Managing cluster lifecycle and configuration
 *
 * Both Proxmox and Talos providers are configured here to enable
 * seamless provisioning of infrastructure and cluster resources.
 *
 * Proxmox connection credentials can be provided via:
 * 1. Direct config (proxmoxConfig.endpoint, proxmoxConfig.apiToken)
 * 2. Terraform variables (proxmox_endpoint, proxmox_api_token)
 * 3. terraform.tfvars file
 * 4. CLI flags (-var="proxmox_endpoint=...")
 */
export class ProxmoxStack extends TerraformStack {
  /** The configured Proxmox provider construct */
  public readonly proxmoxProvider: ProxmoxProviderConstruct;
  /** The configured Talos provider construct */
  public readonly talosProvider: TalosProviderConstruct;
  /** Common tags applied to resources */
  public readonly tags: { [key: string]: string };
  /** Terraform variable for Proxmox endpoint */
  public readonly proxmoxEndpointVar: TerraformVariable;
  /** Terraform variable for Proxmox API token */
  public readonly proxmoxApiTokenVar: TerraformVariable;
  /** The Talos image construct (if VMs are provisioned) */
  public readonly talosImage?: TalosImageConstruct;
  /** Control plane VM constructs (if VMs are provisioned) */
  public readonly controlPlaneVms: TalosVmConstruct[] = [];
  /** Worker VM constructs (if VMs are provisioned with workers) */
  public readonly workerVms: TalosVmConstruct[] = [];
  /** Talos cluster bootstrap construct (if cluster is bootstrapped) */
  public readonly clusterBootstrap?: TalosClusterBootstrapConstruct;
  /** Teleport VM construct (if Teleport is enabled) */
  public readonly teleportVm?: TeleportVmConstruct;

  constructor(scope: Construct, id: string, config: ProxmoxStackConfig) {
    super(scope, id);

    this.tags = {
      environment: config.environment,
      "managed-by": "cdktf",
      project: "waddle-infra",
    };

    // Define Terraform variables for Proxmox credentials
    // These can be set via terraform.tfvars, CLI flags, or TF_VAR_ env vars
    this.proxmoxEndpointVar = new TerraformVariable(this, "proxmox_endpoint", {
      type: "string",
      description: "Proxmox API endpoint URL (e.g., https://proxmox.waddle.social:8006)",
      default: config.proxmoxConfig?.endpoint,
      sensitive: false,
    });

    this.proxmoxApiTokenVar = new TerraformVariable(this, "proxmox_api_token", {
      type: "string",
      description: "Proxmox API token (format: user@realm!tokenid=secret)",
      default: config.proxmoxConfig?.apiToken,
      sensitive: true,
    });

    // Build the Proxmox provider config using Terraform variables
    // Direct config values serve as defaults for the variables
    const proxmoxProviderConfig: ProxmoxProviderConfig = {
      endpoint: this.proxmoxEndpointVar.stringValue,
      apiToken: this.proxmoxApiTokenVar.stringValue,
      insecure: config.proxmoxConfig?.insecure,
      sshUsername: config.proxmoxConfig?.sshUsername,
      sshPassword: config.proxmoxConfig?.sshPassword,
      sshAgent: config.proxmoxConfig?.sshAgent,
    };

    this.proxmoxProvider = new ProxmoxProviderConstruct(
      this,
      "proxmox-provider",
      proxmoxProviderConfig
    );

    this.talosProvider = new TalosProviderConstruct(
      this,
      "talos-provider",
      config.talosConfig
    );

    // =========================================================================
    // Phase 3: Talos VM Provisioning
    // =========================================================================
    //
    // This section provisions Talos Linux VMs on Proxmox for the Kubernetes cluster.
    // Using 3 control plane nodes follows Talos best practices for HA clusters
    // with odd-number etcd quorum (tolerates 1 node failure).
    //
    // IP allocation strategy: Sequential IPs starting from network.ipStart.
    // Topology labels enable zone-aware scheduling in Kubernetes.
    // Labels are stored in VM description and applied to nodes in Phase 4
    // via Talos machine configuration.
    //
    // =========================================================================

    if (config.talosCluster) {
      const cluster = config.talosCluster;

      // Download Talos image to Proxmox storage
      // imageStorageId must be a datastore supporting 'iso' content type (typically 'local')
      this.talosImage = new TalosImageConstruct(this, "talos-image", {
        nodeName: cluster.nodeName,
        datastoreId: cluster.imageStorageId,
        talosVersion: cluster.talosVersion,
      });

      // Helper to build network configuration for VMs
      const buildNetworkConfig = (nodeIndex: number): { bridge: string; ipv4?: { address: string; gateway: string; netmask: string } } => {
        if (cluster.network.static) {
          const staticNet = cluster.network.static;
          return {
            bridge: cluster.networkBridge,
            ipv4: {
              address: `${staticNet.ipPrefix}.${staticNet.ipStart + nodeIndex}`,
              gateway: staticNet.gateway,
              netmask: staticNet.netmask,
            },
          };
        }
        // DHCP mode - no ipv4 configuration
        return {
          bridge: cluster.networkBridge,
        };
      };

      // Create control plane VMs
      for (let i = 0; i < cluster.controlPlane.count; i++) {
        const nodeIndex = i.toString().padStart(2, "0");

        const vmConstruct = new TalosVmConstruct(this, `talos-cp-${nodeIndex}`, {
          nodeName: cluster.nodeName,
          vmName: `${cluster.clusterName}-cp-${nodeIndex}`,
          imageFileId: this.talosImage.fileId,
          cpu: {
            cores: cluster.controlPlane.cores,
            type: "x86-64-v2-AES",
          },
          memory: {
            dedicated: cluster.controlPlane.memory,
          },
          disk: {
            datastoreId: cluster.storageId,
            size: cluster.controlPlane.diskSize,
          },
          network: buildNetworkConfig(i),
          nodeLabels: {
            "topology.kubernetes.io/region": cluster.topology.region,
            "topology.kubernetes.io/zone": cluster.topology.zone,
            "node-role.kubernetes.io/control-plane": "",
          },
          tags: ["control-plane", config.environment],
          description: `Talos control plane node ${i} for ${cluster.clusterName}`,
        });

        // Ensure image is downloaded before VM creation
        vmConstruct.addDependency(this.talosImage);
        this.controlPlaneVms.push(vmConstruct);
      }

      // Create worker VMs (if configured)
      if (cluster.workers && cluster.workers.count > 0) {
        // Worker IPs start after control plane IPs
        const workerIpOffset = cluster.controlPlane.count;

        for (let i = 0; i < cluster.workers.count; i++) {
          const nodeIndex = i.toString().padStart(2, "0");

          const vmConstruct = new TalosVmConstruct(this, `talos-w-${nodeIndex}`, {
            nodeName: cluster.nodeName,
            vmName: `${cluster.clusterName}-w-${nodeIndex}`,
            imageFileId: this.talosImage.fileId,
            cpu: {
              cores: cluster.workers.cores,
              type: "x86-64-v2-AES",
            },
            memory: {
              dedicated: cluster.workers.memory,
            },
            disk: {
              datastoreId: cluster.storageId,
              size: cluster.workers.diskSize,
            },
            network: buildNetworkConfig(workerIpOffset + i),
            nodeLabels: {
              "topology.kubernetes.io/region": cluster.topology.region,
              "topology.kubernetes.io/zone": cluster.topology.zone,
              "node-role.kubernetes.io/worker": "",
            },
            tags: ["worker", config.environment],
            description: `Talos worker node ${i} for ${cluster.clusterName}`,
          });

          // Ensure image is downloaded before VM creation
          vmConstruct.addDependency(this.talosImage);
          this.workerVms.push(vmConstruct);
        }
      }

      // Terraform outputs for cluster information
      new TerraformOutput(this, "control_plane_ips", {
        value: this.controlPlaneVms.map((vm) => vm.ipAddress),
        description: "IP addresses of control plane nodes (undefined if DHCP)",
      });

      new TerraformOutput(this, "control_plane_vm_ids", {
        value: this.controlPlaneVms.map((vm) => vm.vm.vmId),
        description: "Proxmox VM IDs of control plane nodes",
      });

      if (this.workerVms.length > 0) {
        new TerraformOutput(this, "worker_ips", {
          value: this.workerVms.map((vm) => vm.ipAddress),
          description: "IP addresses of worker nodes (undefined if DHCP)",
        });

        new TerraformOutput(this, "worker_vm_ids", {
          value: this.workerVms.map((vm) => vm.vm.vmId),
          description: "Proxmox VM IDs of worker nodes",
        });
      }

      new TerraformOutput(this, "cluster_endpoint", {
        value: cluster.clusterEndpoint,
        description: "Kubernetes API endpoint URL",
      });

      new TerraformOutput(this, "talos_version", {
        value: cluster.talosVersion,
        description: "Deployed Talos Linux version",
      });

      new TerraformOutput(this, "cluster_name", {
        value: cluster.clusterName,
        description: "Kubernetes cluster name",
      });

      new TerraformOutput(this, "network_mode", {
        value: cluster.network.static ? "static" : "dhcp",
        description: "Network configuration mode (static or dhcp)",
      });

      // =========================================================================
      // Phase 4: Talos Cluster Bootstrap
      // =========================================================================
      //
      // This section bootstraps the Kubernetes cluster on provisioned Talos VMs.
      // The bootstrap process:
      // 1. Generates cluster secrets (certificates, tokens)
      // 2. Creates machine configurations for control plane and worker nodes
      // 3. Applies configurations to each node via Talos API
      // 4. Bootstraps etcd and Kubernetes control plane on first CP node
      // 5. Generates kubeconfig for cluster access
      //
      // CNI is set to 'none' - Cilium will be installed in Phase 6.
      // Node labels from VM metadata are applied via machine.nodeLabels in Talos config.
      //
      // IMPORTANT: Phase 4 requires static IP configuration. DHCP mode is supported
      // only for VM provisioning (Phase 3). IP discovery for DHCP will be implemented
      // in a future release.
      //
      // =========================================================================

      // Phase 4 requires static IP configuration
      if (!cluster.network.static) {
        console.warn(
          "\n⚠️  Phase 4 (Talos Cluster Bootstrap) skipped: DHCP mode is not yet supported for bootstrap.\n" +
          "   DHCP mode is currently supported only for VM provisioning (Phase 3).\n" +
          "   To enable Phase 4, configure static IPs by setting:\n" +
          "   - TALOS_NODE_IP_PREFIX (e.g., '192.168.1')\n" +
          "   - TALOS_NODE_GATEWAY (e.g., '192.168.1.1')\n" +
          "   IP discovery for DHCP mode will be implemented in a future release.\n"
        );
      } else {
        // Validate that all VMs have IP addresses (required for static mode bootstrap)
        const missingCpIps = this.controlPlaneVms
          .map((vm, i) => ({ index: i, ip: vm.ipAddress }))
          .filter((v) => !v.ip);
        const missingWorkerIps = this.workerVms
          .map((vm, i) => ({ index: i, ip: vm.ipAddress }))
          .filter((v) => !v.ip);

        if (missingCpIps.length > 0 || missingWorkerIps.length > 0) {
          const missingCpIndices = missingCpIps.map((v) => v.index).join(", ");
          const missingWorkerIndices = missingWorkerIps.map((v) => v.index).join(", ");
          const details = [
            missingCpIps.length > 0 ? `Control plane nodes missing IPs: [${missingCpIndices}]` : "",
            missingWorkerIps.length > 0 ? `Worker nodes missing IPs: [${missingWorkerIndices}]` : "",
          ].filter(Boolean).join("; ");

          throw new Error(
            `Phase 4 bootstrap failed: Static IP mode is configured but some VMs are missing IP addresses. ${details}. ` +
            `Ensure network.static configuration is correct and all VMs have assigned IPs.`
          );
        }

        // Build control plane node configs with labels
        const controlPlaneNodeConfigs = this.controlPlaneVms.map((vm, index) => ({
          name: `${cluster.clusterName}-cp-${index.toString().padStart(2, "0")}`,
          ipAddress: vm.ipAddress as string,
          vmId: vm.vm.vmId as number,
          labels: vm.nodeLabels,
        }));

        // Build worker node configs with labels (if workers exist)
        const workerNodeConfigs = this.workerVms.length > 0
          ? this.workerVms.map((vm, index) => ({
              name: `${cluster.clusterName}-w-${index.toString().padStart(2, "0")}`,
              ipAddress: vm.ipAddress as string,
              vmId: vm.vm.vmId as number,
              labels: vm.nodeLabels,
            }))
          : undefined;

        // Determine if workloads should be scheduled on control planes
        const allowSchedulingOnCPs = cluster.allowSchedulingOnControlPlanes ?? (this.workerVms.length === 0);

        // Create the bootstrap construct
        /* BOOTSTRAP DISABLED TEMPORARILY - Local machine cannot reach internal VM IPs
        (this as { clusterBootstrap: TalosClusterBootstrapConstruct }).clusterBootstrap = new TalosClusterBootstrapConstruct(this, "cluster-bootstrap", {
          clusterName: cluster.clusterName,
          clusterEndpoint: cluster.clusterEndpoint,
          talosVersion: cluster.talosVersion,
          controlPlaneNodes: controlPlaneNodeConfigs,
          workerNodes: workerNodeConfigs,
          kubernetesVersion: cluster.kubernetesVersion,
          clusterDomain: cluster.clusterDomain ?? "cluster.local",
          clusterNetwork: cluster.clusterNetwork ?? "10.244.0.0/16",
          serviceNetwork: cluster.serviceNetwork ?? "10.96.0.0/12",
          cniConfig: { name: cluster.cni ?? "none" },
          installDisk: cluster.installDisk ?? "/dev/sda",
          allowSchedulingOnControlPlanes: allowSchedulingOnCPs,
        });

        // Ensure VMs are created before bootstrap
        this.controlPlaneVms.forEach((vm) => {
          this.clusterBootstrap!.node.addDependency(vm);
        });
        this.workerVms.forEach((vm) => {
          this.clusterBootstrap!.node.addDependency(vm);
        });

        // Phase 4 outputs
        new TerraformOutput(this, "kubeconfig_raw", {
          value: this.clusterBootstrap!.kubeconfigRaw,
          sensitive: true,
          description: "Kubernetes cluster kubeconfig (base64 encoded)",
        });

        new TerraformOutput(this, "talosconfig_raw", {
          value: this.clusterBootstrap!.talosconfigRaw,
          sensitive: true,
          description: "Talos client configuration (base64 encoded)",
        });

        new TerraformOutput(this, "kubernetes_version", {
          value: cluster.kubernetesVersion ?? "Talos default",
          description: "Deployed Kubernetes version",
        });

        new TerraformOutput(this, "cluster_ready", {
          value: "true",
          description: "Cluster bootstrap status",
        });
        */
      }
    }

    // =========================================================================
    // Phase 5: Teleport Secure Access
    // =========================================================================
    //
    // This section provisions a Teleport VM for zero-trust access to Proxmox
    // and Kubernetes infrastructure.
    //
    // Teleport provides:
    // - Application Access for Proxmox web UI (port 8006)
    // - SSH Access for Proxmox host (port 22)
    // - Kubernetes Service access via Kube Agent (deployed in Phase 6+)
    //
    // Architecture:
    // - Single public IP on Teleport Proxy (ports 443, 3024)
    // - All other services accessed through reverse tunnels
    // - Comprehensive audit logging and session recording
    //
    // After deployment:
    // 1. SSH to Teleport VM and install Teleport (see docs/teleport-setup.md)
    // 2. Create first admin user: tctl users add admin --roles=editor,access
    // 3. Configure Proxmox Application and SSH Access
    // 4. Deploy Teleport Kube Agent after Cilium installation (Phase 6+)
    //
    // =========================================================================

    if (config.teleportConfig?.enabled) {
      const teleport = config.teleportConfig;

      // Create the Teleport VM construct
      const teleportVm = new TeleportVmConstruct(this, "teleport-vm", {
        nodeName: teleport.nodeName,
        vmName: teleport.vmName ?? "teleport",
        storageId: teleport.storageId,
        imageStorageId: teleport.imageStorageId,
        networkBridge: teleport.networkBridge,
        cpu: {
          cores: teleport.cores ?? 2,
        },
        memory: teleport.memory ?? 4096,
        diskSize: teleport.diskSize ?? 50,
        network: {
          ipv4: {
            address: teleport.ipAddress,
            gateway: teleport.gateway,
            netmask: teleport.netmask ?? "24",
          },
        },
        sshKeys: teleport.sshKeys,
        teleportDomain: teleport.teleportDomain,
        letsencryptEmail: teleport.letsencryptEmail,
        teleportVersion: teleport.teleportVersion ?? "latest",
        tags: [config.environment],
        environment: config.environment,
      });

      // Store reference for external access
      (this as { teleportVm: TeleportVmConstruct }).teleportVm = teleportVm;

      // Add dependency on Talos image if it exists (ensures proper ordering)
      if (this.talosImage) {
        teleportVm.addDependency(this.talosImage);
      }

      // Terraform outputs for Teleport VM
      new TerraformOutput(this, "teleport_vm_id", {
        value: teleportVm.vm.vmId,
        description: "Proxmox VM ID for Teleport",
      });

      new TerraformOutput(this, "teleport_ip", {
        value: teleportVm.ipAddress,
        description: "IP address of Teleport VM",
      });

      new TerraformOutput(this, "teleport_domain", {
        value: teleportVm.teleportDomain,
        description: "Public domain for Teleport",
      });

      new TerraformOutput(this, "teleport_web_ui", {
        value: `https://${teleportVm.teleportDomain}`,
        description: "Teleport web UI URL",
      });

      new TerraformOutput(this, "teleport_letsencrypt_email", {
        value: teleportVm.letsencryptEmail,
        description: "Let's Encrypt email for ACME",
      });

      new TerraformOutput(this, "teleport_version", {
        value: teleportVm.teleportVersion,
        description: "Teleport version to install",
      });
    }
  }
}
