global
    log /dev/log local0
    maxconn 4096
    daemon

defaults
    log     global
    mode    tcp
    option  tcplog
    option  dontlognull
    timeout connect 5s
    timeout client  30s
    timeout server  30s

frontend ft_ssl
    bind *:443
    mode tcp
    tcp-request inspect-delay 5s
    tcp-request content accept if { req_ssl_hello_type 1 }

    use_backend bk_teleport if { req_ssl_sni -i teleport.${domain} }
    use_backend bk_teleport if { req_ssl_sni -i proxmox.${domain} }
    use_backend bk_k8s_ingress if { req_ssl_sni -m end .${domain} }
    default_backend bk_drop

backend bk_teleport
    mode tcp
    server teleport ${teleport_ip}:3080

backend bk_k8s_ingress
    mode tcp
    server cilium_gw ${cilium_gw_ip}:443

backend bk_drop
    mode tcp

frontend ft_http
    bind *:80
    mode tcp
    default_backend bk_k8s_ingress_http

backend bk_k8s_ingress_http
    mode tcp
    server cilium_gw ${cilium_gw_ip}:80
