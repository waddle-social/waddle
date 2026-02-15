variable "node_name" {
  description = "Proxmox node name"
  type        = string
}

variable "internal_cidr" {
  description = "Internal bridge IP with CIDR (e.g. 10.10.0.1/24)"
  type        = string
}

variable "operator_ip" {
  description = "Operator public IP for temporary SSH access"
  type        = string
}
