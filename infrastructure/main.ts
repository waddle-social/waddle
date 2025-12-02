/**
 * Waddle Infrastructure - Main Entry Point
 *
 * This is the main entry point for the Waddle Infrastructure CDKTF application.
 * It provisions a Talos Kubernetes cluster on Proxmox using:
 * - bpg/proxmox provider for VM provisioning
 * - siderolabs/talos provider for cluster bootstrapping
 *
 * Proxmox credentials can be provided via (in order of precedence):
 * 1. Terraform variables: -var="proxmox_endpoint=..." -var="proxmox_api_token=..."
 * 2. terraform.tfvars file in cdktf.out/stacks/waddle-infra/
 * 3. TF_VAR_proxmox_endpoint and TF_VAR_proxmox_api_token environment variables
 * 4. Node.js environment variables (used as defaults for TF vars):
 *    - PROXMOX_VE_ENDPOINT: Proxmox API endpoint URL
 *    - PROXMOX_VE_API_TOKEN: Proxmox API token (format: user@realm!tokenid=secret)
 *
 * Optional environment variables (Node.js level):
 * - PROXMOX_VE_INSECURE: Skip TLS verification (default: false)
 * - PROXMOX_VE_SSH_USERNAME: SSH username for file operations
 * - PROXMOX_VE_SSH_PASSWORD: SSH password
 * - ENVIRONMENT: Deployment environment (default: development)
 *
 * Talos VM provisioning environment variables (Phase 3):
 * VM provisioning is enabled when PROXMOX_NODE_NAME is set.
 *
 * Required for VM provisioning:
 * - PROXMOX_NODE_NAME: Target Proxmox node name
 * - TALOS_CLUSTER_ENDPOINT: Kubernetes API endpoint URL
 * - TALOS_NODE_IP_PREFIX: IP address prefix (e.g., '192.168.1')
 * - TALOS_NODE_GATEWAY: Network gateway IP
 *
 * Optional (with defaults):
 * - PROXMOX_STORAGE_ID: Storage for VM disks (default: 'local-lvm')
 * - PROXMOX_NETWORK_BRIDGE: Network bridge (default: 'vmbr0')
 * - TALOS_VERSION: Talos version (default: 'v1.11.5')
 * - TALOS_CLUSTER_NAME: Cluster name (default: 'waddle-cluster')
 * - TALOS_NODE_IP_START: Starting IP suffix (default: '101')
 * - TALOS_NODE_NETMASK: Network CIDR suffix (default: '24')
 * - TALOS_CONTROL_PLANE_COUNT: Number of control planes (default: '3')
 * - TALOS_CONTROL_PLANE_CORES: CPU cores per node (default: '4')
 * - TALOS_CONTROL_PLANE_MEMORY: Memory in MB (default: '8192')
 * - TALOS_CONTROL_PLANE_DISK_SIZE: Disk size in GB (default: '50')
 * - TALOS_TOPOLOGY_REGION: Topology region label (default: 'proxmox')
 * - TALOS_TOPOLOGY_ZONE: Topology zone label (default: 'zone-1')
 *
 * Talos cluster bootstrap environment variables (Phase 4):
 * Optional (with defaults):
 * - TALOS_KUBERNETES_VERSION: Kubernetes version (default: Talos default)
 * - TALOS_CLUSTER_DOMAIN: Cluster domain (default: 'cluster.local')
 * - TALOS_CLUSTER_NETWORK: Pod network CIDR (default: '10.244.0.0/16')
 * - TALOS_SERVICE_NETWORK: Service network CIDR (default: '10.96.0.0/12')
 * - TALOS_CNI: CNI name (default: 'none' for external CNI)
 * - TALOS_INSTALL_DISK: Installation disk path (default: '/dev/sda')
 * - TALOS_ALLOW_SCHEDULING_ON_CONTROL_PLANES: Allow workloads on CPs (default: auto)
 *
 * Teleport secure access environment variables (Phase 5):
 * Teleport VM provisioning is enabled when TELEPORT_ENABLED=true.
 *
 * Required for Teleport:
 * - TELEPORT_ENABLED: Set to 'true' to enable Teleport VM provisioning
 * - TELEPORT_DOMAIN: Public FQDN for Teleport (e.g., 'teleport.waddle.social')
 * - TELEPORT_LETSENCRYPT_EMAIL: Email for Let's Encrypt ACME registration
 * - TELEPORT_IP_ADDRESS: Static IP for Teleport VM
 * - TELEPORT_GATEWAY: Network gateway for Teleport VM
 * - TELEPORT_SSH_KEYS: Comma-separated SSH public keys for initial access
 *
 * Optional (with defaults):
 * - TELEPORT_NODE_NAME: Proxmox node (defaults to PROXMOX_NODE_NAME)
 * - TELEPORT_STORAGE_ID: Storage for VM disk (defaults to PROXMOX_STORAGE_ID)
 * - TELEPORT_NETWORK_BRIDGE: Network bridge (defaults to PROXMOX_NETWORK_BRIDGE)
 * - TELEPORT_VM_NAME: VM name in Proxmox (default: 'teleport')
 * - TELEPORT_CORES: CPU cores (default: 2)
 * - TELEPORT_MEMORY: Memory in MB (default: 4096)
 * - TELEPORT_DISK_SIZE: Disk size in GB (default: 50)
 * - TELEPORT_NETMASK: Network CIDR suffix (default: 24)
 * - TELEPORT_VERSION: Teleport version (default: 'latest')
 */

import { config } from "dotenv";
config();

import { App } from "cdktf";
import { ProxmoxStack, TalosClusterConfig, TeleportConfig } from "./lib/stacks";
import { getProxmoxConfigFromEnv } from "./lib/providers";

/**
 * Returns the value of an environment variable or undefined if not set.
 */
function getEnvOrUndefined(key: string): string | undefined {
  const value = process.env[key];
  return value && value.trim() !== "" ? value : undefined;
}

/**
 * Validates that an IP prefix matches the expected format for /24 networks.
 * Format: three octets separated by dots (e.g., '192.168.1')
 */
function validateIpPrefix(prefix: string): boolean {
  const pattern = /^\d{1,3}\.\d{1,3}\.\d{1,3}$/;
  if (!pattern.test(prefix)) {
    return false;
  }
  const octets = prefix.split(".").map(Number);
  return octets.every((octet) => octet >= 0 && octet <= 255);
}

/**
 * Builds TalosClusterConfig from environment variables.
 * Returns undefined if PROXMOX_NODE_NAME is not set (VM provisioning disabled).
 *
 * Network modes:
 * - Static IP: Set TALOS_NODE_IP_PREFIX, TALOS_NODE_GATEWAY, and optionally TALOS_NODE_IP_START
 * - DHCP: Omit TALOS_NODE_IP_PREFIX and TALOS_NODE_GATEWAY (or leave them empty)
 */
function buildTalosClusterConfig(): TalosClusterConfig | undefined {
  const nodeName = getEnvOrUndefined("PROXMOX_NODE_NAME");

  // VM provisioning is only enabled when PROXMOX_NODE_NAME is set
  if (!nodeName) {
    return undefined;
  }

  // TALOS_CLUSTER_ENDPOINT is always required
  const clusterEndpoint = getEnvOrUndefined("TALOS_CLUSTER_ENDPOINT");
  if (!clusterEndpoint) {
    console.warn(
      "Warning: PROXMOX_NODE_NAME is set but TALOS_CLUSTER_ENDPOINT is missing.\n" +
        "  VM provisioning will be skipped."
    );
    return undefined;
  }

  // Static IP variables are optional - when omitted, DHCP mode is used
  const ipPrefix = getEnvOrUndefined("TALOS_NODE_IP_PREFIX");
  const gateway = getEnvOrUndefined("TALOS_NODE_GATEWAY");

  // Build network configuration based on available variables
  let networkConfig: TalosClusterConfig["network"];

  if (ipPrefix && gateway) {
    // Static IP mode - validate IP prefix format
    if (!validateIpPrefix(ipPrefix)) {
      console.error(
        `Error: TALOS_NODE_IP_PREFIX must be a valid /24 prefix (three octets, e.g., '192.168.1').\n` +
          `  Received: '${ipPrefix}'\n` +
          `  Only /24 networks are currently supported.`
      );
      return undefined;
    }

    networkConfig = {
      static: {
        ipPrefix,
        ipStart: parseInt(getEnvOrUndefined("TALOS_NODE_IP_START") ?? "101", 10),
        gateway,
        netmask: getEnvOrUndefined("TALOS_NODE_NETMASK") ?? "24",
      },
    };
    console.log(`Network mode: Static IP (prefix: ${ipPrefix}, start: ${networkConfig.static!.ipStart})`);
  } else if (ipPrefix || gateway) {
    // Partial static config - warn and skip
    console.warn(
      "Warning: Partial static IP configuration detected.\n" +
        "  For static IPs: Set both TALOS_NODE_IP_PREFIX and TALOS_NODE_GATEWAY\n" +
        "  For DHCP: Leave both variables unset\n" +
        "  VM provisioning will be skipped."
    );
    return undefined;
  } else {
    // DHCP mode
    networkConfig = {};
    console.log("Network mode: DHCP (no static IP configuration provided)");
  }

  // Storage configuration
  const storageId = getEnvOrUndefined("PROXMOX_STORAGE_ID") ?? "local-lvm";
  const imageStorageId = getEnvOrUndefined("PROXMOX_IMAGE_STORAGE_ID") ?? "local";

  // Worker node configuration (optional)
  const workerCount = parseInt(getEnvOrUndefined("TALOS_WORKER_COUNT") ?? "0", 10);
  const workers = workerCount > 0
    ? {
        count: workerCount,
        cores: parseInt(getEnvOrUndefined("TALOS_WORKER_CORES") ?? "2", 10),
        memory: parseInt(getEnvOrUndefined("TALOS_WORKER_MEMORY") ?? "4096", 10),
        diskSize: parseInt(getEnvOrUndefined("TALOS_WORKER_DISK_SIZE") ?? "50", 10),
      }
    : undefined;

  // Parse allowSchedulingOnControlPlanes - undefined means auto-detect
  const allowSchedulingStr = getEnvOrUndefined("TALOS_ALLOW_SCHEDULING_ON_CONTROL_PLANES");
  const allowSchedulingOnControlPlanes = allowSchedulingStr === "true" ? true 
    : allowSchedulingStr === "false" ? false 
    : undefined;

  return {
    nodeName,
    storageId,
    imageStorageId,
    networkBridge: getEnvOrUndefined("PROXMOX_NETWORK_BRIDGE") ?? "vmbr0",
    talosVersion: getEnvOrUndefined("TALOS_VERSION") ?? "v1.11.5",
    clusterName: getEnvOrUndefined("TALOS_CLUSTER_NAME") ?? "waddle-cluster",
    clusterEndpoint,
    controlPlane: {
      count: parseInt(getEnvOrUndefined("TALOS_CONTROL_PLANE_COUNT") ?? "3", 10),
      cores: parseInt(getEnvOrUndefined("TALOS_CONTROL_PLANE_CORES") ?? "4", 10),
      memory: parseInt(getEnvOrUndefined("TALOS_CONTROL_PLANE_MEMORY") ?? "8192", 10),
      diskSize: parseInt(getEnvOrUndefined("TALOS_CONTROL_PLANE_DISK_SIZE") ?? "50", 10),
    },
    workers,
    network: networkConfig,
    topology: {
      region: getEnvOrUndefined("TALOS_TOPOLOGY_REGION") ?? "proxmox",
      zone: getEnvOrUndefined("TALOS_TOPOLOGY_ZONE") ?? "zone-1",
    },
    // Phase 4: Bootstrap configuration
    kubernetesVersion: getEnvOrUndefined("TALOS_KUBERNETES_VERSION"),
    clusterDomain: getEnvOrUndefined("TALOS_CLUSTER_DOMAIN") ?? "cluster.local",
    clusterNetwork: getEnvOrUndefined("TALOS_CLUSTER_NETWORK") ?? "10.244.0.0/16",
    serviceNetwork: getEnvOrUndefined("TALOS_SERVICE_NETWORK") ?? "10.96.0.0/12",
    cni: getEnvOrUndefined("TALOS_CNI") ?? "none",
    installDisk: getEnvOrUndefined("TALOS_INSTALL_DISK") ?? "/dev/sda",
    allowSchedulingOnControlPlanes,
  };
}

/**
 * Validates that a string is a valid FQDN (Fully Qualified Domain Name).
 */
function validateFqdn(domain: string): boolean {
  const fqdnPattern = /^(?!-)[A-Za-z0-9-]{1,63}(?<!-)(\.[A-Za-z0-9-]{1,63})*$/;
  return fqdnPattern.test(domain) && domain.includes(".");
}

/**
 * Builds TeleportConfig from environment variables.
 * Returns undefined if TELEPORT_ENABLED is not set to 'true'.
 */
function buildTeleportConfig(): TeleportConfig | undefined {
  const enabled = getEnvOrUndefined("TELEPORT_ENABLED");

  // Teleport provisioning is only enabled when TELEPORT_ENABLED=true
  if (enabled !== "true") {
    return undefined;
  }

  // Required variables
  const teleportDomain = getEnvOrUndefined("TELEPORT_DOMAIN");
  const letsencryptEmail = getEnvOrUndefined("TELEPORT_LETSENCRYPT_EMAIL");
  const ipAddress = getEnvOrUndefined("TELEPORT_IP_ADDRESS");
  const gateway = getEnvOrUndefined("TELEPORT_GATEWAY");
  const sshKeysStr = getEnvOrUndefined("TELEPORT_SSH_KEYS");

  // Validate required variables
  const missingVars: string[] = [];
  if (!teleportDomain) missingVars.push("TELEPORT_DOMAIN");
  if (!letsencryptEmail) missingVars.push("TELEPORT_LETSENCRYPT_EMAIL");
  if (!ipAddress) missingVars.push("TELEPORT_IP_ADDRESS");
  if (!gateway) missingVars.push("TELEPORT_GATEWAY");
  if (!sshKeysStr) missingVars.push("TELEPORT_SSH_KEYS");

  if (missingVars.length > 0) {
    console.warn(
      `\n⚠️  Teleport is enabled but missing required variables: ${missingVars.join(", ")}\n` +
      "   Teleport VM provisioning will be skipped.\n"
    );
    return undefined;
  }

  // Validate FQDN
  if (!validateFqdn(teleportDomain!)) {
    console.warn(
      `\n⚠️  TELEPORT_DOMAIN must be a valid FQDN (e.g., 'teleport.waddle.social').\n` +
      `   Received: '${teleportDomain}'\n` +
      "   Teleport VM provisioning will be skipped.\n"
    );
    return undefined;
  }

  // Parse SSH keys (comma-separated)
  const sshKeys = sshKeysStr!.split(",").map((key) => key.trim()).filter((key) => key.length > 0);
  if (sshKeys.length === 0) {
    console.warn(
      "\n⚠️  TELEPORT_SSH_KEYS is set but contains no valid keys.\n" +
      "   Teleport VM provisioning will be skipped.\n"
    );
    return undefined;
  }

  // Get optional variables with defaults (inherit from Proxmox/Talos config)
  const nodeName = getEnvOrUndefined("TELEPORT_NODE_NAME") ?? getEnvOrUndefined("PROXMOX_NODE_NAME");
  const storageId = getEnvOrUndefined("TELEPORT_STORAGE_ID") ?? getEnvOrUndefined("PROXMOX_STORAGE_ID") ?? "local-lvm";
  const imageStorageId = getEnvOrUndefined("TELEPORT_IMAGE_STORAGE_ID") ?? getEnvOrUndefined("PROXMOX_IMAGE_STORAGE_ID") ?? "local";
  const networkBridge = getEnvOrUndefined("TELEPORT_NETWORK_BRIDGE") ?? getEnvOrUndefined("PROXMOX_NETWORK_BRIDGE") ?? "vmbr0";

  if (!nodeName) {
    console.warn(
      "\n⚠️  Teleport is enabled but no Proxmox node name is configured.\n" +
      "   Set TELEPORT_NODE_NAME or PROXMOX_NODE_NAME.\n" +
      "   Teleport VM provisioning will be skipped.\n"
    );
    return undefined;
  }

  // Check for Talos cluster configuration - warn if Teleport enabled without cluster
  const talosClusterEndpoint = getEnvOrUndefined("TALOS_CLUSTER_ENDPOINT");
  if (!talosClusterEndpoint) {
    console.warn(
      "\n⚠️  Teleport is enabled but no Talos cluster is configured.\n" +
      "   Teleport will only secure Proxmox access. Kubernetes integration\n" +
      "   requires a Talos cluster (Phase 3/4) and Teleport Kube Agent (Phase 6+).\n"
    );
  }

  const teleportConfig: TeleportConfig = {
    enabled: true,
    nodeName,
    vmName: getEnvOrUndefined("TELEPORT_VM_NAME") ?? "teleport",
    storageId,
    imageStorageId,
    networkBridge,
    ipAddress: ipAddress!,
    gateway: gateway!,
    netmask: getEnvOrUndefined("TELEPORT_NETMASK") ?? "24",
    teleportDomain: teleportDomain!,
    letsencryptEmail: letsencryptEmail!,
    sshKeys,
    cores: parseInt(getEnvOrUndefined("TELEPORT_CORES") ?? "2", 10),
    memory: parseInt(getEnvOrUndefined("TELEPORT_MEMORY") ?? "4096", 10),
    diskSize: parseInt(getEnvOrUndefined("TELEPORT_DISK_SIZE") ?? "50", 10),
    teleportVersion: getEnvOrUndefined("TELEPORT_VERSION") ?? "latest",
  };

  console.log(
    `\nTeleport configuration:\n` +
    `  Domain: ${teleportConfig.teleportDomain}\n` +
    `  IP Address: ${teleportConfig.ipAddress}\n` +
    `  Node: ${teleportConfig.nodeName}\n` +
    `  Version: ${teleportConfig.teleportVersion}\n`
  );

  return teleportConfig;
}

function main(): void {
  const environment = process.env.ENVIRONMENT ?? "development";

  // Try to load Proxmox config from env vars (used as defaults for TF variables)
  // If env vars are not set, the TF variables will be required at apply time
  let proxmoxConfig;
  try {
    proxmoxConfig = getProxmoxConfigFromEnv();
  } catch {
    // Environment variables not set - TF variables will be required at apply time
    proxmoxConfig = undefined;
  }

  // Load Talos cluster configuration (VM provisioning is optional)
  const talosCluster = buildTalosClusterConfig();

  // Load Teleport configuration (secure access is optional)
  const teleportConfig = buildTeleportConfig();

  const app = new App();

  new ProxmoxStack(app, "waddle-infra", {
    proxmoxConfig,
    environment,
    talosCluster,
    teleportConfig,
  });

  app.synth();
}

main();
