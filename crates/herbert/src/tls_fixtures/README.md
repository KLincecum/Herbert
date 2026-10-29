# Test-only TLS fixtures

Generated once by `make_tls_fixtures.sh` with OpenSSL 3.5.9, EC P-256.
They are used only by `transport_trust` tests. No system trusts them and their keys protect nothing.

- `self-signed.pem/.key`: self-signed `localhost` / `127.0.0.1`, valid 2026-01-01 to 2126-01-01.
- `other-host.pem/.key`: self-signed, SAN `other.invalid` only (name-mismatch case).
- `expired.pem/.key`: self-signed `localhost`, valid 2020-01-01 to 2021-01-01.
- `private-ca-root.pem`: test CA (its key was discarded after signing).
- `private-ca-chain.pem` + `private-ca-leaf.key`: `localhost` leaf signed by the test CA, followed by the CA.
