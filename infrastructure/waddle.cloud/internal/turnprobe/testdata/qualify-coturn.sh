#!/usr/bin/env bash
# Qualify authenticated UDP and TLS relay against a real, pinned coturn image.
# The echo peer and temporary credentials exist only in an isolated network.
set -euo pipefail

module_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/waddle-turnprobe.XXXXXX")"
fixture_name="waddle-turnprobe-$$"
image="coturn/coturn:4.18.0@sha256:bbefd3e1fdfdc0d58770fe01b581fd8b00d9f3a5580d00acb77cf719a6bc78e3"
cleanup() {
  if [[ "${TURNPROBE_KEEP_FIXTURE:-0}" == 1 ]]; then
    printf 'Retained qualification fixture: network=%s turn=%s-turn peer=%s-peer client=%s-client files=%s\n' \
      "${fixture_name}" "${fixture_name}" "${fixture_name}" "${fixture_name}" "${fixture_dir}"
    return
  fi
  docker rm -f "${fixture_name}-turn" "${fixture_name}-peer" "${fixture_name}-client" >/dev/null 2>&1 || true
  docker network rm "${fixture_name}" >/dev/null 2>&1 || true
  rm -rf "${fixture_dir}"
}
trap cleanup EXIT

cat >"${fixture_dir}/cert.conf" <<'EOF'
[req]
distinguished_name=dn
x509_extensions=ext
prompt=no
[dn]
CN=localhost
[ext]
subjectAltName=DNS:localhost,IP:127.0.0.1,IP:172.30.171.3
basicConstraints=critical,CA:TRUE
keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign
EOF
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -config "${fixture_dir}/cert.conf" \
  -keyout "${fixture_dir}/tls.key" -out "${fixture_dir}/tls.crt" >/dev/null 2>&1
chmod 644 "${fixture_dir}/tls.key"
arch="$(docker version --format '{{.Server.Arch}}')"
GOOS=linux GOARCH="${arch}" CGO_ENABLED=0 go build -o "${fixture_dir}/echo" "${module_dir}/internal/turnprobe/testdata/echo.go"
(cd "${module_dir}" && GOOS=linux GOARCH="${arch}" CGO_ENABLED=0 go test -c -o "${fixture_dir}/turnprobe.test" ./internal/turnprobe)
cat >"${fixture_dir}/turnserver.conf" <<'EOF'
listening-port=3478
tls-listening-port=5349
listening-ip=172.30.171.3
relay-ip=172.30.171.3
min-port=49152
max-port=49160
realm=turnprobe.waddle.test
use-auth-secret
static-auth-secret=isolated-turnprobe-secret
stale-nonce=1
cert=/fixture/tls.crt
pkey=/fixture/tls.key
no-tcp-relay
no-multicast-peers
denied-peer-ip=0.0.0.0-255.255.255.255
denied-peer-ip=::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff
allowed-peer-ip=172.30.171.2
fingerprint
log-file=stdout
simple-log
EOF
docker network create --subnet 172.30.171.0/24 "${fixture_name}" >/dev/null
docker create --name "${fixture_name}-peer" --network "${fixture_name}" --ip 172.30.171.2 \
  --entrypoint /fixture/echo "${image}" >/dev/null
docker create --name "${fixture_name}-turn" --network "${fixture_name}" --ip 172.30.171.3 \
  --entrypoint turnserver "${image}" -c /fixture/turnserver.conf >/dev/null
chmod 755 "${fixture_dir}"
docker cp "${fixture_dir}/." "${fixture_name}-peer:/fixture"
docker cp "${fixture_dir}/." "${fixture_name}-turn:/fixture"
docker start "${fixture_name}-peer" "${fixture_name}-turn" >/dev/null
for attempt in {1..20}; do
  if docker logs "${fixture_name}-turn" 2>&1 | grep -q 'TLS1.3'; then
    break
  fi
  if ! docker inspect -f '{{.State.Running}}' "${fixture_name}-turn" | grep -q true; then
    docker logs "${fixture_name}-turn"
    exit 1
  fi
  sleep 0.2
done
export TURNPROBE_TEST_USERNAME="$(($(date +%s) + 600)):turnprobe"
export TURNPROBE_TEST_PASSWORD="$(printf '%s' "${TURNPROBE_TEST_USERNAME}" | openssl dgst -sha1 -hmac isolated-turnprobe-secret -binary | openssl base64 -A)"
docker create --name "${fixture_name}-client" --network "${fixture_name}" \
  -e TURNPROBE_TEST_UDP_ADDR=172.30.171.3:3478 -e TURNPROBE_TEST_TLS_ADDR=172.30.171.3:5349 \
  -e TURNPROBE_TEST_PEER=172.30.171.2:3479 -e TURNPROBE_TEST_CA=/fixture/tls.crt \
  -e TURNPROBE_TEST_STALE_NONCE=1 \
  -e TURNPROBE_TEST_USERNAME -e TURNPROBE_TEST_PASSWORD \
  --entrypoint /fixture/turnprobe.test "${image}" -test.v >/dev/null
docker cp "${fixture_dir}/." "${fixture_name}-client:/fixture"
docker start -a "${fixture_name}-client"
status="$(docker inspect -f '{{.State.ExitCode}}' "${fixture_name}-client")"
exit "${status}"
