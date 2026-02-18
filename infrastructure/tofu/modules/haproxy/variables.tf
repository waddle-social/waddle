variable "node_name" {
  description = "Proxmox node name"
  type        = string
}

variable "datastore_id" {
  description = "Proxmox datastore for VM disks"
  type        = string
}

variable "vm_id" {
  description = "VM ID for HAProxy"
  type        = number
  default     = 100
}

variable "public_ip" {
  description = "Public IP address for HAProxy VM"
  type        = string
}

variable "public_gateway" {
  description = "Public gateway IP"
  type        = string
}

variable "internal_ip" {
  description = "Internal IP for HAProxy on vmbr1"
  type        = string
}

variable "internal_gateway" {
  description = "Internal gateway IP"
  type        = string
}

variable "teleport_internal_ip" {
  description = "Teleport VM IP on internal network"
  type        = string
}

variable "cilium_gateway_vip" {
  description = "Cilium Gateway VIP"
  type        = string
}

variable "ssh_public_key" {
  description = "SSH public key for VM access"
  type        = string
}

variable "domain" {
  description = "Base domain"
  type        = string
}
