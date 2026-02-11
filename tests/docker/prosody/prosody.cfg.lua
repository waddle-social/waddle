admins = {}

daemonize = false
allow_registration = true
registration_throttle_max = 0
c2s_require_encryption = false
s2s_require_encryption = false

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
    "muc";
    "muc_mam";
    "ping";
    "time";
    "version";
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
