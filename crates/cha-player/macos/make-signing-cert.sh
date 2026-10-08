#!/bin/sh
# Make a self-signed code-signing identity, "Cha Player Dev", in the login
# keychain, so bundle.sh signs every build the same way and macOS keeps
# Cha Player's privacy permissions (Input Monitoring) across rebuilds.
#
# For development on this Mac only: other people need a Developer ID build
# (milestone C3). macOS asks for your password once, to trust the
# certificate for code signing. Run it yourself:
#   crates/cha-player/macos/make-signing-cert.sh
set -eu
name="${1:-Cha Player Dev}"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -qF "\"$name\""; then
    echo "\"$name\" is already a code-signing identity"
    exit 0
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cat > "$dir/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

# macOS's own LibreSSL: its PKCS#12 is what `security import` reads.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -config "$dir/cert.cnf" -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
pass=$(/usr/bin/openssl rand -hex 16)
/usr/bin/openssl pkcs12 -export -name "$name" -inkey "$dir/key.pem" -in "$dir/cert.pem" \
    -out "$dir/identity.p12" -passout "pass:$pass"

# The key may be used by codesign without asking each time.
security import "$dir/identity.p12" -k "$keychain" -P "$pass" -T /usr/bin/codesign
# Trusted for code signing only (asks for your password).
security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$dir/cert.pem"

security find-identity -v -p codesigning | grep -F "\"$name\""
echo "Done. bundle.sh now signs with \"$name\"; grant Input Monitoring to the next build once."
