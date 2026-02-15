output "vm_id" {
  description = "HAProxy VM ID"
  value       = proxmox_virtual_environment_vm.haproxy.vm_id
}

output "ipv4_addresses" {
  description = "HAProxy VM IPv4 addresses"
  value       = proxmox_virtual_environment_vm.haproxy.ipv4_addresses
}

output "debian_image_id" {
  description = "Downloaded Debian cloud image ID (reusable by other modules)"
  value       = proxmox_virtual_environment_download_file.debian_cloud_image.id
}
