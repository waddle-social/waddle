output "vm_id" {
  description = "Teleport VM ID"
  value       = proxmox_virtual_environment_vm.teleport.vm_id
}

output "ipv4_addresses" {
  description = "Teleport VM IPv4 addresses"
  value       = proxmox_virtual_environment_vm.teleport.ipv4_addresses
}
