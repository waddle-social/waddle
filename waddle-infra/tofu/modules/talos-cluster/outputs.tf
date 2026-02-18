output "vm_ids" {
  description = "Talos node VM IDs"
  value       = proxmox_virtual_environment_vm.talos_node[*].vm_id
}

output "node_ips" {
  description = "Talos node IPs"
  value       = var.node_ips
}

output "talos_image_id" {
  description = "Downloaded Talos image ID"
  value       = proxmox_virtual_environment_download_file.talos_image.id
}
