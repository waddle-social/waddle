import { Construct } from "constructs";
import { ProxmoxProvider } from "../../.gen/providers/proxmox/provider";

/**
 * Configuration options for the Proxmox provider.
 */
export interface ProxmoxProviderConfig {
  /** Proxmox API endpoint URL (e.g., https://proxmox.waddle.social:8006) */
  endpoint: string;
  /** API token for authentication (format: user@realm!tokenid=secret) */
  apiToken: string;
  /** Whether to skip TLS certificate verification (default: false) */
  insecure?: boolean;
  /** SSH username for file operations (required for uploading cloud-init snippets) */
  sshUsername?: string;
  /** SSH password for authentication */
  sshPassword?: string;
  /** Whether to use SSH agent for authentication */
  sshAgent?: boolean;
}

/**
 * A reusable construct that configures the Proxmox provider.
 *
 * This construct encapsulates Proxmox provider setup, making it reusable across
 * multiple stacks and testable in isolation. The provider is used for provisioning
 * VMs, managing storage, and uploading cloud-init configurations.
 *
 * SSH credentials are needed when:
 * - Uploading cloud-init snippets to Proxmox storage
 * - Managing files on the Proxmox host
 * - Using file-based provisioning features
 */
export class ProxmoxProviderConstruct extends Construct {
  /** The underlying Proxmox provider instance */
  public readonly provider: ProxmoxProvider;

  constructor(scope: Construct, id: string, config: ProxmoxProviderConfig) {
    super(scope, id);

    this.provider = new ProxmoxProvider(this, "proxmox", {
      endpoint: config.endpoint,
      apiToken: config.apiToken,
      insecure: config.insecure ?? false,
      ssh: config.sshUsername
        ? {
            username: config.sshUsername,
            password: config.sshPassword,
            agent: config.sshAgent ?? false,
          }
        : undefined,
    });
  }
}

/**
 * Loads Proxmox provider configuration from environment variables.
 *
 * Required environment variables:
 * - PROXMOX_VE_ENDPOINT: Proxmox API endpoint URL
 * - PROXMOX_VE_API_TOKEN: API token (format: user@realm!tokenid=secret)
 *
 * Optional environment variables:
 * - PROXMOX_VE_INSECURE: Skip TLS verification (default: false)
 * - PROXMOX_VE_SSH_USERNAME: SSH username for file operations
 * - PROXMOX_VE_SSH_PASSWORD: SSH password
 *
 * @throws Error if required environment variables are missing
 */
export function getProxmoxConfigFromEnv(): ProxmoxProviderConfig {
  const endpoint = process.env.PROXMOX_VE_ENDPOINT;
  const apiToken = process.env.PROXMOX_VE_API_TOKEN;

  if (!endpoint) {
    throw new Error(
      "Missing required environment variable: PROXMOX_VE_ENDPOINT. " +
        "Set it to your Proxmox API endpoint (e.g., https://proxmox.waddle.social:8006)"
    );
  }

  if (!apiToken) {
    throw new Error(
      "Missing required environment variable: PROXMOX_VE_API_TOKEN. " +
        "Set it to your Proxmox API token (format: user@realm!tokenid=secret)"
    );
  }

  const insecureStr = process.env.PROXMOX_VE_INSECURE?.toLowerCase();
  const insecure = insecureStr === "true" || insecureStr === "1";

  return {
    endpoint,
    apiToken,
    insecure,
    sshUsername: process.env.PROXMOX_VE_SSH_USERNAME,
    sshPassword: process.env.PROXMOX_VE_SSH_PASSWORD,
    sshAgent:
      process.env.PROXMOX_VE_SSH_AGENT?.toLowerCase() === "true" ||
      process.env.PROXMOX_VE_SSH_AGENT === "1",
  };
}
