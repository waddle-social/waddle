module:hook("s2sout-created", function (event)
    local session = event.session
    if session.to_host ~= module:get_option_string("fixture_peer_target") then return end
    local config = session.ssl_ctx._builder:clone()
    config:apply({
        certificate = false;
        key = false;
        cafile = module:get_option_string("fixture_peer_ca");
    })
    session.ssl_ctx = assert(config:build())
    session.ssl_cfg = config:final()
    module:log("info", "Fixture uses certificate-less outbound TLS to %s", session.to_host)
end, -1)
