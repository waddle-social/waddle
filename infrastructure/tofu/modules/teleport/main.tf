resource "proxmox_virtual_environment_file" "teleport_cloud_config" {
  content_type = "snippets"
  datastore_id = "local"
  node_name    = var.node_name

  source_raw {
    data = templatefile("${path.module}/cloud-init.cfg", {
      ssh_public_key         = var.ssh_public_key
      domain                 = var.domain
      proxmox_host_ip        = var.proxmox_host_ip
      github_org             = var.github_org
      github_client_id       = var.github_client_id
      github_client_secret   = var.github_client_secret
      talos_vip              = var.talos_vip
      teleport_major_version = "18"
    })
    file_name = "teleport-cloud-init.yaml"
  }
}

resource "proxmox_virtual_environment_vm" "teleport" {
  name      = "teleport"
  node_name = var.node_name
  vm_id     = var.vm_id
  tags      = ["infra", "teleport"]

  stop_on_destroy = true
  started         = true

  agent {
    enabled = true
  }

  cpu {
    cores = 2
    type  = "host"
  }

  memory {
    dedicated = 2048
  }

  disk {
    datastore_id = var.datastore_id
    import_from  = var.debian_image_id
    interface    = "virtio0"
    iothread     = true
    discard      = "on"
    size         = 20
  }

  network_device {
    bridge = "vmbr1"
  }

  initialization {
    ip_config {
      ipv4 {
        address = "${var.internal_ip}/24"
        gateway = var.internal_gateway
      }
    }
    user_data_file_id = proxmox_virtual_environment_file.teleport_cloud_config.id
  }
}
