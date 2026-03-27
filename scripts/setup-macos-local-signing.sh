#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script only runs on macOS." >&2
  exit 1
fi

IDENTITY_NAME="${VOICEX_LOCAL_SIGN_IDENTITY:-VoiceX Local Code Signing}"
KEYCHAIN_PATH="${HOME}/Library/Keychains/login.keychain-db"
PKCS12_PASSWORD="${VOICEX_LOCAL_SIGN_P12_PASSWORD:-voicex-local-sign}"

if security find-identity -v -p codesigning | grep -Fq "${IDENTITY_NAME}"; then
  echo "Code-signing identity already exists: ${IDENTITY_NAME}"
  exit 0
fi

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "${tmp_dir}"
}
trap cleanup EXIT

cat > "${tmp_dir}/openssl.cnf" <<EOF
[ req ]
distinguished_name = req_dn
x509_extensions = req_ext
prompt = no
default_md = sha256

[ req_dn ]
CN = ${IDENTITY_NAME}
O = VoiceX
OU = Local

[ req_ext ]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = codeSigning
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
EOF

openssl genrsa -out "${tmp_dir}/key.pem" 2048 >/dev/null 2>&1
openssl req -x509 -new -nodes -key "${tmp_dir}/key.pem" -days 3650 -out "${tmp_dir}/cert.pem" -config "${tmp_dir}/openssl.cnf" >/dev/null 2>&1

if openssl pkcs12 -help 2>&1 | grep -q -- '-legacy'; then
  openssl pkcs12 -export \
    -legacy \
    -inkey "${tmp_dir}/key.pem" \
    -in "${tmp_dir}/cert.pem" \
    -name "${IDENTITY_NAME}" \
    -out "${tmp_dir}/cert.p12" \
    -passout "pass:${PKCS12_PASSWORD}" >/dev/null 2>&1
else
  openssl pkcs12 -export \
    -inkey "${tmp_dir}/key.pem" \
    -in "${tmp_dir}/cert.pem" \
    -name "${IDENTITY_NAME}" \
    -out "${tmp_dir}/cert.p12" \
    -passout "pass:${PKCS12_PASSWORD}" >/dev/null 2>&1
fi

security import "${tmp_dir}/cert.p12" \
  -k "${KEYCHAIN_PATH}" \
  -P "${PKCS12_PASSWORD}" \
  -T /usr/bin/codesign \
  -T /usr/bin/security >/dev/null

security add-trusted-cert \
  -d \
  -r trustRoot \
  -k "${KEYCHAIN_PATH}" \
  "${tmp_dir}/cert.pem" >/dev/null

if ! security find-identity -v -p codesigning "${KEYCHAIN_PATH}" | grep -Fq "${IDENTITY_NAME}"; then
  echo "Imported certificate, but code-signing identity is still unavailable: ${IDENTITY_NAME}" >&2
  echo "Open Keychain Access and trust this certificate for code signing, then rerun." >&2
  exit 1
fi

echo "Created local macOS code-signing identity: ${IDENTITY_NAME}"
echo "Available code-signing identities:"
security find-identity -v -p codesigning "${KEYCHAIN_PATH}" | grep -F "${IDENTITY_NAME}"
