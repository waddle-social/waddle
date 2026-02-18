#!/usr/bin/env sh
set -eu

prosodyctl --config /etc/prosody/prosody.cfg.lua register alice localhost alice_pass
prosodyctl --config /etc/prosody/prosody.cfg.lua register bob localhost bob_pass
prosodyctl --config /etc/prosody/prosody.cfg.lua register charlie localhost charlie_pass
