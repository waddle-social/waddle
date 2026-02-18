admins = {}

daemonize = false
allow_registration = true
registration_throttle_max = 0
c2s_require_encryption = false
s2s_require_encryption = false
consider_websocket_secure = true
cross_domain_websocket = true
http_ports = { 5280 }
https_ports = {}

modules_enabled = {
    "roster";
    "saslauth";
    "tls";
    "dialback";
    "disco";
    "carbons";
    "pep";
    "register";
    "mam";
    "smacks";
    "ping";
    "time";
    "version";
    "bosh";
    "websocket";
}

storage = "internal"
archive_expires_after = "never"
default_archive_policy = true

VirtualHost "localhost"
    authentication = "internal_hashed"

Component "conference.localhost" "muc"
    modules_enabled = { "muc_mam" }
    muc_room_default_public = true
    muc_room_default_persistent = false
