import { Construct } from "constructs";
import { MachineSecrets } from "../../.gen/providers/talos/machine-secrets";
import { DataTalosMachineConfiguration } from "../../.gen/providers/talos/data-talos-machine-configuration";
import { MachineConfigurationApply } from "../../.gen/providers/talos/machine-configuration-apply";
import { MachineBootstrap } from "../../.gen/providers/talos/machine-bootstrap";
import { ClusterKubeconfig } from "../../.gen/providers/talos/cluster-kubeconfig";
import { DataTalosClientConfiguration } from "../../.gen/providers/talos/data-talos-client-configuration";

/**
 * Node configuration for Talos cluster bootstrap.
 */
export interface TalosNodeConfig {
  /** Node name (e.g., 'waddle-cluster-cp-00') */
  name: string;
  /** Node IP address */
  ipAddress: string;
  /** Proxmox VM ID */
  vmId: number;
  /** Kubernetes node labels to apply via machine configuration */
  labels?: Record<string, string>;
}

/**
 * CNI configuration for Talos machine config.
 */
export interface TalosCniConfig {
  /** CNI name ('none' for external CNI like Cilium, 'flannel' for built-in) */
  name: string;
  /** Optional CNI manifest URLs */
  urls?: string[];
}

/**
 * Configuration for TalosClusterBootstrapConstruct.
 */
export interface TalosClusterBootstrapConfig {
  /** Kubernetes cluster name */
  clusterName: string;
  /** Kubernetes API endpoint URL (e.g., 'https://192.168.1.101:6443') */
  clusterEndpoint: string;
  /** Talos version (e.g., 'v1.11.5') */
  talosVersion: string;
  /** Control plane node configurations */
  controlPlaneNodes: TalosNodeConfig[];
  /** Worker node configurations (optional) */
  workerNodes?: TalosNodeConfig[];
  /** Kubernetes version (optional, uses Talos default if not specified) */
  kubernetesVersion?: string;
  /** Cluster domain (default: 'cluster.local') */
  clusterDomain?: string;
  /** Pod network CIDR (default: '10.244.0.0/16') */
  clusterNetwork?: string;
  /** Service network CIDR (default: '10.96.0.0/12') */
  serviceNetwork?: string;
  /** CNI configuration */
  cniConfig?: TalosCniConfig;
  /** Disk path for Talos installation (default: '/dev/sda') */
  installDisk?: string;
  /** Allow scheduling workloads on control plane nodes (default: auto-detected) */
  allowSchedulingOnControlPlanes?: boolean;
}

/**
 * A construct for bootstrapping a Talos Kubernetes cluster.
 *
 * This construct encapsulates all Talos cluster bootstrap resources:
 * 1. MachineSecrets - Generate cluster secrets (certificates, tokens)
 * 2. DataTalosMachineConfiguration - Generate control plane and worker configs
 * 3. MachineConfigurationApply - Apply configs to each node
 * 4. MachineBootstrap - Bootstrap the cluster on first CP node
 * 5. ClusterKubeconfig - Generate kubeconfig for cluster access
 * 6. DataTalosClientConfiguration - Generate talosconfig for Talos API access
 *
 * CNI is configured to 'none' by default, allowing external CNI (Cilium)
 * to be installed separately in Phase 6.
 *
 * Tunnel Mode:
 * When TALOS_USE_TUNNEL=true, connections are routed through Teleport TCP tunnels
 * instead of directly to node IPs. This enables provisioning from outside the
 * internal network. Set TALOS_TUNNEL_HOST (default: 127.0.0.1) and
 * TALOS_TUNNEL_PORT_START (default: 50001) to configure tunnel endpoints.
 *
 * @see https://www.talos.dev/v1.11/talos-guides/install/virtualized-platforms/proxmox/
 */
export class TalosClusterBootstrapConstruct extends Construct {
  /** Whether tunnel mode is enabled */
  private readonly useTunnel: boolean;
  /** Tunnel host for connections (default: 127.0.0.1) */
  private readonly tunnelHost: string;
  /** Starting port for tunneled connections (default: 50001) */
  private readonly tunnelPortStart: number;

  /** The machine secrets resource */
  public readonly machineSecrets: MachineSecrets;
  /** Control plane machine configuration data source */
  public readonly controlPlaneConfig: DataTalosMachineConfiguration;
  /** Worker machine configuration data source (if workers exist) */
  public readonly workerConfig?: DataTalosMachineConfiguration;
  /** Configuration apply resources for control plane nodes */
  public readonly controlPlaneConfigApply: MachineConfigurationApply[];
  /** Configuration apply resources for worker nodes */
  public readonly workerConfigApply: MachineConfigurationApply[];
  /** Machine bootstrap resource */
  public readonly bootstrap: MachineBootstrap;
  /** Cluster kubeconfig resource */
  public readonly kubeconfig: ClusterKubeconfig;
  /** Talos client configuration data source */
  public readonly talosClientConfig: DataTalosClientConfiguration;
  /** Raw kubeconfig string for output */
  public readonly kubeconfigRaw: string;
  /** Raw talosconfig string for output */
  public readonly talosconfigRaw: string;

  constructor(scope: Construct, id: string, config: TalosClusterBootstrapConfig) {
    super(scope, id);

    // Initialize tunnel mode settings from environment variables
    this.useTunnel = process.env.TALOS_USE_TUNNEL === "true";
    this.tunnelHost = process.env.TALOS_TUNNEL_HOST ?? "127.0.0.1";
    this.tunnelPortStart = parseInt(process.env.TALOS_TUNNEL_PORT_START ?? "50001", 10);

    if (this.useTunnel) {
      console.log(`Tunnel mode enabled: Using ${this.tunnelHost}:${this.tunnelPortStart}+ for Talos API connections`);
    }

    const clusterDomain = config.clusterDomain ?? "cluster.local";
    const clusterNetwork = config.clusterNetwork ?? "10.244.0.0/16";
    const serviceNetwork = config.serviceNetwork ?? "10.96.0.0/12";
    const installDisk = config.installDisk ?? "/dev/sda";
    const cniName = config.cniConfig?.name ?? "none";

    // Determine if workloads should be scheduled on control planes
    const hasWorkers = config.workerNodes && config.workerNodes.length > 0;
    const allowSchedulingOnControlPlanes = config.allowSchedulingOnControlPlanes ?? !hasWorkers;

    // Step 1: Generate machine secrets
    this.machineSecrets = new MachineSecrets(this, "secrets", {
      talosVersion: config.talosVersion,
    });

    // Extract labels from first control plane node (all CP nodes have same labels)
    const controlPlaneLabels = config.controlPlaneNodes[0]?.labels;

    // Build config patches for control plane nodes
    const controlPlaneConfigPatches = this.buildConfigPatches({
      installDisk,
      clusterDomain,
      clusterNetwork,
      serviceNetwork,
      cniName,
      cniUrls: config.cniConfig?.urls,
      allowSchedulingOnControlPlanes,
      machineType: "controlplane",
      nodeLabels: controlPlaneLabels,
    });

    // Step 2: Generate control plane machine configuration
    this.controlPlaneConfig = new DataTalosMachineConfiguration(this, "cp-config", {
      clusterName: config.clusterName,
      clusterEndpoint: config.clusterEndpoint,
      machineType: "controlplane",
      machineSecrets: {
        certs: {
          etcd: {
            cert: this.machineSecrets.machineSecrets.certs.etcd.cert,
            key: this.machineSecrets.machineSecrets.certs.etcd.key,
          },
          k8S: {
            cert: this.machineSecrets.machineSecrets.certs.k8S.cert,
            key: this.machineSecrets.machineSecrets.certs.k8S.key,
          },
          k8SAggregator: {
            cert: this.machineSecrets.machineSecrets.certs.k8SAggregator.cert,
            key: this.machineSecrets.machineSecrets.certs.k8SAggregator.key,
          },
          k8SServiceaccount: {
            key: this.machineSecrets.machineSecrets.certs.k8SServiceaccount.key,
          },
          os: {
            cert: this.machineSecrets.machineSecrets.certs.os.cert,
            key: this.machineSecrets.machineSecrets.certs.os.key,
          },
        },
        cluster: {
          id: this.machineSecrets.machineSecrets.cluster.id,
          secret: this.machineSecrets.machineSecrets.cluster.secret,
        },
        secrets: {
          bootstrapToken: this.machineSecrets.machineSecrets.secrets.bootstrapToken,
          secretboxEncryptionSecret: this.machineSecrets.machineSecrets.secrets.secretboxEncryptionSecret,
        },
        trustdinfo: {
          token: this.machineSecrets.machineSecrets.trustdinfo.token,
        },
      },
      talosVersion: config.talosVersion,
      kubernetesVersion: config.kubernetesVersion,
      configPatches: controlPlaneConfigPatches,
      docs: false,
      examples: false,
    });

    // Step 2b: Generate worker machine configuration (if workers exist)
    if (hasWorkers) {
      // Extract labels from first worker node (all worker nodes have same labels)
      const workerLabels = config.workerNodes?.[0]?.labels;

      const workerConfigPatches = this.buildConfigPatches({
        installDisk,
        clusterDomain,
        clusterNetwork,
        serviceNetwork,
        cniName,
        cniUrls: config.cniConfig?.urls,
        allowSchedulingOnControlPlanes: false,
        machineType: "worker",
        nodeLabels: workerLabels,
      });

      this.workerConfig = new DataTalosMachineConfiguration(this, "worker-config", {
        clusterName: config.clusterName,
        clusterEndpoint: config.clusterEndpoint,
        machineType: "worker",
        machineSecrets: {
          certs: {
            etcd: {
              cert: this.machineSecrets.machineSecrets.certs.etcd.cert,
              key: this.machineSecrets.machineSecrets.certs.etcd.key,
            },
            k8S: {
              cert: this.machineSecrets.machineSecrets.certs.k8S.cert,
              key: this.machineSecrets.machineSecrets.certs.k8S.key,
            },
            k8SAggregator: {
              cert: this.machineSecrets.machineSecrets.certs.k8SAggregator.cert,
              key: this.machineSecrets.machineSecrets.certs.k8SAggregator.key,
            },
            k8SServiceaccount: {
              key: this.machineSecrets.machineSecrets.certs.k8SServiceaccount.key,
            },
            os: {
              cert: this.machineSecrets.machineSecrets.certs.os.cert,
              key: this.machineSecrets.machineSecrets.certs.os.key,
            },
          },
          cluster: {
            id: this.machineSecrets.machineSecrets.cluster.id,
            secret: this.machineSecrets.machineSecrets.cluster.secret,
          },
          secrets: {
            bootstrapToken: this.machineSecrets.machineSecrets.secrets.bootstrapToken,
            secretboxEncryptionSecret: this.machineSecrets.machineSecrets.secrets.secretboxEncryptionSecret,
          },
          trustdinfo: {
            token: this.machineSecrets.machineSecrets.trustdinfo.token,
          },
        },
        talosVersion: config.talosVersion,
        kubernetesVersion: config.kubernetesVersion,
        configPatches: workerConfigPatches,
        docs: false,
        examples: false,
      });
    }

    // Step 3: Apply machine configuration to control plane nodes
    this.controlPlaneConfigApply = config.controlPlaneNodes.map((node, index) => {
      const endpoint = this.getNodeEndpoint(node.ipAddress, index);
      const apply = new MachineConfigurationApply(this, `cp-apply-${index.toString().padStart(2, "0")}`, {
        clientConfiguration: {
          caCertificate: this.machineSecrets.clientConfiguration.caCertificate,
          clientCertificate: this.machineSecrets.clientConfiguration.clientCertificate,
          clientKey: this.machineSecrets.clientConfiguration.clientKey,
        },
        machineConfigurationInput: this.controlPlaneConfig.machineConfiguration,
        nodeAttribute: node.ipAddress,
        endpoint: endpoint,
        timeouts: {
          create: "10m",
          update: "10m",
        },
      });
      return apply;
    });

    // Step 3b: Apply machine configuration to worker nodes
    // Worker node indices are offset by control plane count for tunnel port mapping
    this.workerConfigApply = [];
    if (hasWorkers && config.workerNodes && this.workerConfig) {
      const cpCount = config.controlPlaneNodes.length;
      this.workerConfigApply = config.workerNodes.map((node, index) => {
        const endpoint = this.getNodeEndpoint(node.ipAddress, cpCount + index);
        const apply = new MachineConfigurationApply(this, `worker-apply-${index.toString().padStart(2, "0")}`, {
          clientConfiguration: {
            caCertificate: this.machineSecrets.clientConfiguration.caCertificate,
            clientCertificate: this.machineSecrets.clientConfiguration.clientCertificate,
            clientKey: this.machineSecrets.clientConfiguration.clientKey,
          },
          machineConfigurationInput: this.workerConfig!.machineConfiguration,
          nodeAttribute: node.ipAddress,
          endpoint: endpoint,
          timeouts: {
            create: "10m",
            update: "10m",
          },
        });
        return apply;
      });
    }

    // Step 4: Bootstrap the cluster on the first control plane node
    const firstCpNode = config.controlPlaneNodes[0];
    const firstCpEndpoint = this.getNodeEndpoint(firstCpNode.ipAddress, 0);
    this.bootstrap = new MachineBootstrap(this, "bootstrap", {
      clientConfiguration: {
        caCertificate: this.machineSecrets.clientConfiguration.caCertificate,
        clientCertificate: this.machineSecrets.clientConfiguration.clientCertificate,
        clientKey: this.machineSecrets.clientConfiguration.clientKey,
      },
      nodeAttribute: firstCpNode.ipAddress,
      endpoint: firstCpEndpoint,
      timeouts: {
        create: "10m",
      },
    });

    // Bootstrap depends on all config apply resources
    this.controlPlaneConfigApply.forEach((apply) => {
      this.bootstrap.node.addDependency(apply);
    });
    this.workerConfigApply.forEach((apply) => {
      this.bootstrap.node.addDependency(apply);
    });

    // Step 5: Generate kubeconfig after bootstrap
    this.kubeconfig = new ClusterKubeconfig(this, "kubeconfig", {
      clientConfiguration: {
        caCertificate: this.machineSecrets.clientConfiguration.caCertificate,
        clientCertificate: this.machineSecrets.clientConfiguration.clientCertificate,
        clientKey: this.machineSecrets.clientConfiguration.clientKey,
      },
      nodeAttribute: firstCpNode.ipAddress,
      endpoint: firstCpEndpoint,
      timeouts: {
        create: "10m",
      },
    });
    this.kubeconfig.node.addDependency(this.bootstrap);

    // Step 6: Generate talosconfig
    const nodeEndpoints = config.controlPlaneNodes.map((n) => n.ipAddress);
    this.talosClientConfig = new DataTalosClientConfiguration(this, "talosconfig", {
      clusterName: config.clusterName,
      clientConfiguration: {
        caCertificate: this.machineSecrets.clientConfiguration.caCertificate,
        clientCertificate: this.machineSecrets.clientConfiguration.clientCertificate,
        clientKey: this.machineSecrets.clientConfiguration.clientKey,
      },
      endpoints: nodeEndpoints,
      nodes: nodeEndpoints,
    });

    // Expose raw configs for outputs
    this.kubeconfigRaw = this.kubeconfig.kubeconfigRaw;
    this.talosconfigRaw = this.talosClientConfig.talosConfig;
  }

  /**
   * Get the endpoint for a node, using tunnel mode if enabled.
   * In tunnel mode, connections are routed through local ports (50001, 50002, etc.)
   * that are tunneled via Teleport to the actual node IPs.
   *
   * @param nodeIpAddress - The actual IP address of the node
   * @param nodeIndex - The index of the node (0-based)
   * @returns The endpoint to use for connections
   */
  private getNodeEndpoint(nodeIpAddress: string, nodeIndex: number): string {
    if (this.useTunnel) {
      const port = this.tunnelPortStart + nodeIndex;
      return `${this.tunnelHost}:${port}`;
    }
    return nodeIpAddress;
  }

  /**
   * Build config patches for machine configuration.
   */
  private buildConfigPatches(options: {
    installDisk: string;
    clusterDomain: string;
    clusterNetwork: string;
    serviceNetwork: string;
    cniName: string;
    cniUrls?: string[];
    allowSchedulingOnControlPlanes: boolean;
    machineType: "controlplane" | "worker";
    nodeLabels?: Record<string, string>;
  }): string[] {
    const patches: string[] = [];

    // Install disk patch
    patches.push(JSON.stringify({
      machine: {
        install: {
          disk: options.installDisk,
        },
      },
    }));

    // Network configuration patch (CNI, pod/service CIDRs)
    const clusterPatch: Record<string, unknown> = {
      cluster: {
        network: {
          dnsDomain: options.clusterDomain,
          podSubnets: [options.clusterNetwork],
          serviceSubnets: [options.serviceNetwork],
          cni: {
            name: options.cniName,
            urls: options.cniUrls,
          },
        },
      },
    };

    // Control plane scheduling patch
    if (options.machineType === "controlplane") {
      (clusterPatch.cluster as Record<string, unknown>).allowSchedulingOnControlPlanes = options.allowSchedulingOnControlPlanes;
    }

    patches.push(JSON.stringify(clusterPatch));

    // Node labels patch (machine.nodeLabels)
    if (options.nodeLabels && Object.keys(options.nodeLabels).length > 0) {
      patches.push(JSON.stringify({
        machine: {
          nodeLabels: options.nodeLabels,
        },
      }));
    }

    return patches;
  }
}
