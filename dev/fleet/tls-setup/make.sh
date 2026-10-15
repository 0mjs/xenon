#!/bin/sh
# Throwaway certificates for the fleet's TLS tests, in dev/fleet/tls (never use
# them anywhere else): a CA, a server certificate for localhost and 127.0.0.1,
# and a client certificate for the user `xenon_cert`, who logs in with it.
set -eu
out="$(cd "$(dirname "$0")/.." && pwd)/tls"
[ -f "$out/client.key" ] && exit 0
mkdir -p "$out"
cd "$out"
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Xenon fleet CA" \
  -keyout ca.key -out ca.pem 2>/dev/null
# 800 days: macOS refuses server certificates valid for longer than 825.
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > server.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 800 \
  -extfile server.ext -out server.pem 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=xenon_cert" -keyout client.key -out client.csr 2>/dev/null
printf 'extendedKeyUsage=clientAuth\n' > client.ext
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 800 \
  -extfile client.ext -out client.pem 2>/dev/null
rm -f ./*.csr ./*.ext ./*.srl
# Readable by the database servers in their containers.
chmod 644 ./*.pem ./*.key
