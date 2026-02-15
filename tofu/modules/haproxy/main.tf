resource "proxmox_virtual_environment_download_file" "debian_cloud_image" {
  content_type = "import"
  datastore_id = "local"
  node_name    = var.node_name
  url          = "https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2"
  file_name    = "debian-12-generic-amd64.qcow2"
}

resource "proxmox_virtual_environment_file" "haproxy_cloud_config" {
  content_type = "snippets"
  datastore_id = "local"
  node_name    = var.node_name

  source_raw {
    data = templatefile("${path.module}/cloud-init.cfg", {
      ssh_public_key = var.ssh_public_key
      haproxy_cfg = templatefile("${path.module}/haproxy.cfg.tpl", {
        domain       = var.domain
        teleport_ip  = var.teleport_internal_ip
        cilium_gw_ip = var.cilium_gateway_vip
      })
    })
    file_name = "haproxy-cloud-init.yaml"
  }
}

resource "proxmox_virtual_environment_vm" "haproxy" {
  name      = "haproxy"
  node_name = var.node_name
  vm_id     = var.vm_id
  tags      = ["infra", "haproxy"]

  stop_on_destroy = true
  started         = true

  agent {
    enabled = true
  }

  cpu {
    cores = 1
    type  = "host"
  }

  memory {
    dedicated = 512
  }

  disk {
    datastore_id = var.datastore_id
    import_from  = proxmox_virtual_environment_download_file.debian_cloud_image.id
    interface    = "virtio0"
    iothread     = true
    discard      = "on"
    size         = 8
  }

  network_device {
    bridge = "vmbr0"
  }

  network_device {
    bridge = "vmbr1"
  }

  initialization {
    ip_config {
      ipv4 {
        address = "${var.public_ip}/32"
        gateway = var.public_gateway
      }
    }
    ip_config {
      ipv4 {
        address = "${var.internal_ip}/24"
        gateway = var.internal_gateway
      }
    }
    user_data_file_id = proxmox_virtual_environment_file.haproxy_cloud_config.id
  }
}
