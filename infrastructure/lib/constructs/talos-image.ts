import { Construct } from "constructs";
import { VirtualEnvironmentDownloadFile } from "../../.gen/providers/proxmox/virtual-environment-download-file";

/**
 * Configuration for the TalosImageConstruct.
 */
export interface TalosImageConfig {
  /** Target Proxmox node name (e.g., 'pve', 'proxmox01') */
  nodeName: string;
  /** Proxmox storage ID for image download (e.g., 'local') */
  datastoreId: string;
  /** Talos version to download (e.g., 'v1.11.5') */
  talosVersion: string;
  /**
   * Optional Image Factory schematic ID for custom images.
   * Default: vanilla Talos image (no extensions).
   * Generate custom schematics at: https://factory.talos.dev/
   */
  schematicId?: string;
  /** CPU architecture (default: 'amd64') */
  architecture?: string;
}

/**
 * A construct for downloading Talos OS images to Proxmox storage.
 *
 * Downloads Talos nocloud images from Image Factory (factory.talos.dev) and
 * stores them in Proxmox storage for VM provisioning.
 *
 * Image Factory URL structure:
 * https://factory.talos.dev/image/{schematicId}/{version}/nocloud-{arch}.qcow2
 *
 * The schematic ID defines the image configuration (extensions, customizations).
 * The default schematic ID produces a vanilla Talos image suitable for most
 * Proxmox deployments.
 *
 * Note: Proxmox treats qcow2 files as 'iso' content type for cloud images.
 * This is correct behavior for the bpg/proxmox provider.
 *
 * @see https://factory.talos.dev/ for custom schematic generation
 * @see https://www.talos.dev/v1.11/talos-guides/install/virtualized-platforms/proxmox/
 */
export class TalosImageConstruct extends Construct {
  /** The underlying download file resource */
  public readonly fileResource: VirtualEnvironmentDownloadFile;
  /** Computed file ID for VM disk attachment (format: datastoreId:iso/fileName) */
  public readonly fileId: string;
  /** The downloaded file name */
  public readonly fileName: string;

  constructor(scope: Construct, id: string, config: TalosImageConfig) {
    super(scope, id);

    const architecture = config.architecture ?? "amd64";
    // Default schematic ID for vanilla Talos (no extensions)
    const schematicId =
      config.schematicId ??
      "ce4c980550dd2ab1b17bbf2b08801c7eb59418eafe8f279833297925d67c7515";

    this.fileName = `talos-${config.talosVersion}-nocloud-${architecture}.img`;
    const downloadUrl = `https://factory.talos.dev/image/${schematicId}/${config.talosVersion}/nocloud-${architecture}.qcow2`;

    this.fileResource = new VirtualEnvironmentDownloadFile(this, "image", {
      contentType: "iso",
      datastoreId: config.datastoreId,
      nodeName: config.nodeName,
      url: downloadUrl,
      fileName: this.fileName,
      overwrite: true,
    });

    this.fileId = `${config.datastoreId}:iso/${this.fileName}`;
  }
}
