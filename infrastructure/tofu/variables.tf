variable "proxmox_endpoint" {
  description = "Proxmox API endpoint URL (e.g. https://10.10.0.1:8006)"
  type        = string
}

variable "proxmox_api_token" {
  description = "Proxmox API token in format user@realm!token-name=secret"
  type        = string
  sensitive   = true
}

variable "proxmox_node_name" {
  description = "Name of the Proxmox node"
  type        = string
  default     = "pve"
}

variable "proxmox_ssh_username" {
  description = "SSH username for Proxmox host (used by provider for file operations)"
  type        = string
  default     = "root"
}

variable "proxmox_ssh_host" {
  description = "SSH host for Proxmox (public IP, since internal IP is unreachable from workstation)"
  type        = string
}

variable "public_ip" {
  description = "Public IP address assigned to the Scaleway Elastic Metal server"
  type        = string
}

variable "public_gateway" {
  description = "Public gateway IP for vmbr0"
  type        = string
}

variable "ssh_public_key" {
  description = "SSH public key for VM access"
  type        = string
}

variable "internal_network" {
  description = "Internal network CIDR for vmbr1"
  type        = string
  default     = "10.10.0.0/24"
}

variable "internal_gateway" {
  description = "Internal gateway IP (Proxmox host on vmbr1)"
  type        = string
  default     = "10.10.0.1"
}

variable "haproxy_internal_ip" {
  description = "HAProxy VM internal IP on vmbr1"
  type        = string
  default     = "10.10.0.3"
}

variable "teleport_internal_ip" {
  description = "Teleport VM internal IP on vmbr1"
  type        = string
  default     = "10.10.0.2"
}

variable "talos_node_ips" {
  description = "Static IPs for the 3 Talos nodes"
  type        = list(string)
  default     = ["10.10.0.10", "10.10.0.11", "10.10.0.12"]
}

variable "talos_vip" {
  description = "Talos shared VIP for Kubernetes API"
  type        = string
  default     = "10.10.0.20"
}

variable "cilium_gateway_vip" {
  description = "Cilium Gateway L2 VIP"
  type        = string
  default     = "10.10.0.30"
}

variable "domain" {
  description = "Base domain"
  type        = string
  default     = "waddle.social"
}

variable "teleport_github_org" {
  description = "GitHub organization for Teleport SSO"
  type        = string
  default     = "waddle-social"
}

variable "teleport_github_client_id" {
  description = "GitHub OAuth App client ID for Teleport"
  type        = string
  sensitive   = true
  default     = ""
}

variable "teleport_github_client_secret" {
  description = "GitHub OAuth App client secret for Teleport"
  type        = string
  sensitive   = true
  default     = ""
}

variable "datastore_id" {
  description = "Proxmox datastore for VM disks"
  type        = string
  default     = "local-lvm"
}

variable "operator_ip" {
  description = "Operator's current public IP for temporary SSH access during bootstrap"
  type        = string
}

variable "talos_schematic_id" {
  description = "Talos Image Factory schematic ID (includes qemu-guest-agent + iscsi-tools extensions)"
  type        = string
  default     = "dc7b152cb3ea99b821fcb7340ce7168313ce393d663740b791c36f6e95fc8586"
}
