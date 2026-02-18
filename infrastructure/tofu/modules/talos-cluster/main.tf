locals {
  node_names      = ["talos-cp1", "talos-cp2", "talos-cp3"]
  talos_image_url = "https://factory.talos.dev/image/${var.talos_schematic_id}/${var.talos_version}/nocloud-amd64.qcow2"
}

resource "proxmox_virtual_environment_download_file" "talos_image" {
  content_type        = "import"
  datastore_id        = "local"
  node_name           = var.node_name
  url                 = local.talos_image_url
  file_name           = "talos-${var.talos_version}-nocloud-amd64.qcow2"
  overwrite           = false
  overwrite_unmanaged = true
}

resource "proxmox_virtual_environment_vm" "talos_node" {
  count = 3

  name      = local.node_names[count.index]
  node_name = var.node_name
  vm_id     = var.base_vm_id + count.index
  tags      = ["infra", "talos", "k8s"]

  stop_on_destroy = true
  started         = true

  agent {
    enabled = true
  }

  cpu {
    cores = 3
    type  = "host"
  }

  memory {
    dedicated = 8192
  }

  disk {
    datastore_id = var.datastore_id
    import_from  = proxmox_virtual_environment_download_file.talos_image.id
    interface    = "virtio0"
    iothread     = true
    discard      = "on"
    size         = 10
  }

  network_device {
    bridge = "vmbr1"
  }

  initialization {
    datastore_id = "local"
    ip_config {
      ipv4 {
        address = "${var.node_ips[count.index]}/24"
        gateway = var.internal_gateway
      }
    }
    dns {
      servers = ["1.1.1.1", "8.8.8.8"]
    }
  }
}
