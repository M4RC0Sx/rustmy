#!/bin/sh
# Generates throwaway TLS material for the test servers into
# tests/docker/certs (git-ignored; tests/docker is mounted at /work). Runs once:
# later starts reuse the existing files. Delete the directory to regenerate.
#
#   ca.pem / ca-key.pem          test CA that signs everything below
#   server.pem / server-key.pem  server cert for localhost, 127.0.0.1, ::1
#   client.pem / client-key.pem  client cert (CN=rustmy_x509) for REQUIRE X509
#   other-ca.pem                 an unrelated CA, to test rejection
set -eu

mkdir -p /work/certs
cd /work/certs
if [ -f ca.pem ] && [ -f server.pem ] && [ -f client.pem ] && [ -f other-ca.pem ]; then
	echo "test certificates already present"
	exit 0
fi
apk add --no-cache openssl >/dev/null

days=3650
# Leaf certificates stay within the 825 days that Apple platforms accept, so
# operating-system verification fails for the real reason (unknown CA).
leaf_days=825
openssl req -x509 -newkey rsa:2048 -nodes -days "$days" \
	-subj "/CN=rustmy test CA" -keyout ca-key.pem -out ca.pem
openssl req -x509 -newkey rsa:2048 -nodes -days "$days" \
	-subj "/CN=rustmy untrusted CA" -keyout other-ca-key.pem -out other-ca.pem

issue() { # name, subject, extended key usage, extra extensions
	printf 'basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=%s\n%s\n' \
		"$3" "$4" >"$1.ext"
	openssl req -newkey rsa:2048 -nodes -subj "$2" -keyout "$1-key.pem" -out "$1.csr"
	openssl x509 -req -in "$1.csr" -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
		-days "$leaf_days" -extfile "$1.ext" -out "$1.pem"
}
issue server "/CN=localhost" serverAuth "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1"
issue client "/CN=rustmy_x509" clientAuth ""

rm -f ./*.csr ./*.ext ./*.srl
# The server processes run as an unprivileged user and must read their key.
chmod 644 ./*.pem
echo "test certificates generated"
