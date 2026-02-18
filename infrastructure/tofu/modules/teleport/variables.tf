variable "node_name" {
  description = "Proxmox node name"
  type        = string
}

variable "datastore_id" {
  description = "Proxmox datastore for VM disks"
  type        = string
}

variable "vm_id" {
  description = "VM ID for Teleport"
  type        = number
  default     = 101
}

variable "internal_ip" {
  description = "Internal IP for Teleport on vmbr1"
  type        = string
}

variable "internal_gateway" {
  description = "Internal gateway IP"
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

variable "proxmox_host_ip" {
  description = "Proxmox host IP on internal network (for app access)"
  type        = string
}

variable "github_org" {
  description = "GitHub organization for SSO"
  type        = string
}

variable "github_client_id" {
  description = "GitHub OAuth App client ID"
  type        = string
  sensitive   = true
}

variable "github_client_secret" {
  description = "GitHub OAuth App client secret"
  type        = string
  sensitive   = true
}

variable "talos_vip" {
  description = "Talos VIP for Kubernetes API access"
  type        = string
}

variable "debian_image_id" {
  description = "ID of the downloaded Debian cloud image (from haproxy module)"
  type        = string
}
