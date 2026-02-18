FROM prosody/prosody:0.11.9

USER root

COPY prosody.cfg.lua /etc/prosody/prosody.cfg.lua
COPY create-users.sh /usr/local/bin/create-users.sh

RUN chmod +x /usr/local/bin/create-users.sh \
    && /usr/local/bin/create-users.sh \
    && chown -R prosody:prosody /var/lib/prosody /etc/prosody/certs

USER prosody

ENTRYPOINT ["prosody", "-F"]
