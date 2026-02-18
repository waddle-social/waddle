variable "node_name" {
  description = "Proxmox node name"
  type        = string
}

variable "datastore_id" {
  description = "Proxmox datastore for VM disks"
  type        = string
}

variable "base_vm_id" {
  description = "Starting VM ID for Talos nodes"
  type        = number
  default     = 110
}

variable "node_ips" {
  description = "Static IPs for Talos nodes"
  type        = list(string)
}

variable "internal_gateway" {
  description = "Internal gateway IP"
  type        = string
}

variable "talos_version" {
  description = "Talos Linux version"
  type        = string
  default     = "v1.12.4"
}

variable "talos_schematic_id" {
  description = "Talos Image Factory schematic ID (with qemu-guest-agent + iscsi-tools extensions)"
  type        = string
}
