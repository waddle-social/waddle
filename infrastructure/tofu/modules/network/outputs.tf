output "internal_bridge_name" {
  description = "Name of the internal bridge"
  value       = proxmox_virtual_environment_network_linux_bridge.vmbr1.name
}
