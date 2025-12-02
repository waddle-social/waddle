import { Construct } from "constructs";
import { TalosProvider } from "../../.gen/providers/talos/provider";

/**
 * Configuration options for the Talos provider.
 *
 * The Talos provider only supports imageFactoryUrl at the provider level.
 * All other configuration (endpoints, talosconfig) is handled at the
 * resource level (e.g., talos_machine_configuration_apply).
 */
export interface TalosProviderConfig {
  /** URL of Image Factory for generating schematics (default: https://factory.talos.dev) */
  imageFactoryUrl?: string;
}

/**
 * A reusable construct that configures the Talos provider.
 *
 * This construct provides a consistent interface for the Talos provider, which
 * is used for:
 * - Generating Talos machine configurations (controlplane and worker)
 * - Bootstrapping Kubernetes clusters on Talos nodes
 * - Retrieving kubeconfig and talosconfig for cluster access
 * - Managing Talos cluster secrets
 *
 * The provider itself requires minimal configuration. Connection details
 * (endpoints, talosconfig) are specified at the resource level, not here.
 */
export class TalosProviderConstruct extends Construct {
  /** The underlying Talos provider instance */
  public readonly provider: TalosProvider;

  constructor(scope: Construct, id: string, config?: TalosProviderConfig) {
    super(scope, id);

    this.provider = new TalosProvider(this, "talos", {
      imageFactoryUrl: config?.imageFactoryUrl,
    });
  }
}
