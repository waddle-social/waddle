resource "proxmox_virtual_environment_network_linux_bridge" "vmbr1" {
  node_name = var.node_name
  name      = "vmbr1"
  address   = var.internal_cidr
  comment   = "Internal network for K8s, Teleport, iSCSI"
  autostart = true
}

resource "proxmox_virtual_environment_cluster_firewall" "cluster" {
  enabled       = true
  ebtables      = false
  input_policy  = "DROP"
  output_policy = "ACCEPT"
}

resource "proxmox_virtual_environment_firewall_rules" "host" {
  node_name = var.node_name

  depends_on = [proxmox_virtual_environment_cluster_firewall.cluster]

  rule {
    type    = "in"
    action  = "ACCEPT"
    comment = "Allow all traffic from internal bridge"
    iface   = "vmbr1"
  }

  rule {
    type    = "in"
    action  = "ACCEPT"
    comment = "Temporary SSH from operator IP (remove after Teleport setup)"
    source  = var.operator_ip
    dport   = "22"
    proto   = "tcp"
  }
}
