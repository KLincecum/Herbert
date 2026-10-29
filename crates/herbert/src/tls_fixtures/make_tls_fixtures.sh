#!/usr/bin/env bash
# Generate Herbert's test-only TLS fixtures (no real trust; keys protect nothing).
set -euo pipefail
OPENSSL=/c/Tools/Workbench/packages/git-v2.56.0.windows.2/usr/bin/openssl.exe
OUT="$1"
mkdir -p "$OUT"
cd "$OUT"
cat > leaf.cnf <<'EOF'
[req]
distinguished_name = dn
prompt = no
[dn]
CN = localhost
[leaf]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost,IP:127.0.0.1
[other]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:other.invalid
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
EOF
key() { "$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1" 2>/dev/null; }
key self-signed.key
"$OPENSSL" req -new -x509 -config leaf.cnf -extensions leaf -key self-signed.key -subj /CN=localhost \
  -not_before 20260101000000Z -not_after 21260101000000Z -out self-signed.pem
key other-host.key
"$OPENSSL" req -new -x509 -config leaf.cnf -extensions other -key other-host.key -subj /CN=other.invalid \
  -not_before 20260101000000Z -not_after 21260101000000Z -out other-host.pem
key expired.key
"$OPENSSL" req -new -x509 -config leaf.cnf -extensions leaf -key expired.key -subj /CN=localhost \
  -not_before 20200101000000Z -not_after 20210101000000Z -out expired.pem
key private-ca-root.key
"$OPENSSL" req -new -x509 -config leaf.cnf -extensions ca -key private-ca-root.key -subj "/CN=Herbert Test Private CA/O=Herbert tests" \
  -not_before 20260101000000Z -not_after 21260101000000Z -out private-ca-root.pem
key private-ca-leaf.key
"$OPENSSL" req -new -config leaf.cnf -key private-ca-leaf.key -subj /CN=localhost -out leaf.csr
"$OPENSSL" x509 -req -in leaf.csr -CA private-ca-root.pem -CAkey private-ca-root.key -CAcreateserial \
  -extfile leaf.cnf -extensions leaf -not_before 20260101000000Z -not_after 21260101000000Z -out private-ca-leaf.pem
cat private-ca-leaf.pem private-ca-root.pem > private-ca-chain.pem
rm -f leaf.csr private-ca-root.key private-ca-root.srl private-ca-leaf.pem
cat > README.md <<'EOF'
# Test-only TLS fixtures

Generated once by `make_tls_fixtures.sh` with OpenSSL 3.5.9, EC P-256.
They are used only by `transport_trust` tests. No system trusts them and their keys protect nothing.

- `self-signed.pem/.key`: self-signed `localhost` / `127.0.0.1`, valid 2026-01-01 to 2126-01-01.
- `other-host.pem/.key`: self-signed, SAN `other.invalid` only (name-mismatch case).
- `expired.pem/.key`: self-signed `localhost`, valid 2020-01-01 to 2021-01-01.
- `private-ca-root.pem`: test CA (its key was discarded after signing).
- `private-ca-chain.pem` + `private-ca-leaf.key`: `localhost` leaf signed by the test CA, followed by the CA.
EOF
rm -f leaf.cnf
ls
