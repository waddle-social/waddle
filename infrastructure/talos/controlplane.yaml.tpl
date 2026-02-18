machine:
  type: controlplane
  network:
    hostname: ${hostname}
    interfaces:
      - interface: eth0
        addresses:
          - ${node_ip}/24
        routes:
          - network: 0.0.0.0/0
            gateway: ${gateway}
        vip:
          ip: ${vip}
    nameservers:
      - 1.1.1.1
      - 8.8.8.8
  install:
    disk: /dev/vda
    wipe: false
  kubelet:
    extraArgs:
      rotate-server-certificates: "true"
    nodeIP:
      validSubnets:
        - 10.10.0.0/24
  features:
    kubePrism:
      enabled: true
      port: 7445

cluster:
  controlPlane:
    endpoint: https://${vip}:6443
    scheduler:
      extraArgs:
        bind-address: "0.0.0.0"
    controllerManager:
      extraArgs:
        bind-address: "0.0.0.0"
  clusterName: waddle-cluster
  network:
    cni:
      name: none
    podSubnets:
      - 10.244.0.0/16
    serviceSubnets:
      - 10.96.0.0/12
  proxy:
    disabled: true
  allowSchedulingOnControlPlanes: true
  etcd:
    advertisedSubnets:
      - 10.10.0.0/24
