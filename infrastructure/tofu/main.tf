provider "proxmox" {
  endpoint  = var.proxmox_endpoint
  api_token = var.proxmox_api_token
  insecure  = true

  ssh {
    agent    = true
    username = var.proxmox_ssh_username
    node {
      name    = var.proxmox_node_name
      address = var.proxmox_ssh_host
    }
  }
}

module "network" {
  source = "./modules/network"

  node_name     = var.proxmox_node_name
  internal_cidr = "${var.internal_gateway}/24"
  operator_ip   = var.operator_ip
}

module "haproxy" {
  source = "./modules/haproxy"

  depends_on = [module.network]

  node_name            = var.proxmox_node_name
  datastore_id         = var.datastore_id
  public_ip            = var.public_ip
  public_gateway       = var.public_gateway
  internal_ip          = var.haproxy_internal_ip
  internal_gateway     = var.internal_gateway
  teleport_internal_ip = var.teleport_internal_ip
  cilium_gateway_vip   = var.cilium_gateway_vip
  ssh_public_key       = var.ssh_public_key
  domain               = var.domain
}

module "teleport" {
  source = "./modules/teleport"

  depends_on = [module.network]

  node_name            = var.proxmox_node_name
  datastore_id         = var.datastore_id
  internal_ip          = var.teleport_internal_ip
  internal_gateway     = var.internal_gateway
  ssh_public_key       = var.ssh_public_key
  domain               = var.domain
  proxmox_host_ip      = var.internal_gateway
  github_org           = var.teleport_github_org
  github_client_id     = var.teleport_github_client_id
  github_client_secret = var.teleport_github_client_secret
  talos_vip            = var.talos_vip
  debian_image_id      = module.haproxy.debian_image_id
}

module "talos_cluster" {
  source = "./modules/talos-cluster"

  depends_on = [module.network]

  node_name          = var.proxmox_node_name
  datastore_id       = var.datastore_id
  node_ips           = var.talos_node_ips
  internal_gateway   = var.internal_gateway
  talos_version      = "v1.12.4"
  talos_schematic_id = var.talos_schematic_id
}
