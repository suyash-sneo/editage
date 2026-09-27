#!/bin/sh
# Regenerates the interoperability fixtures with the reference Go
# implementation of age (https://github.com/FiloSottile/age), independently
# of this application. Requires `age`, `age-keygen` and `expect`.
#
# All fixture contents are obvious dummy text. The passphrase is public:
#   fixture-passphrase
set -eu
cd "$(dirname "$0")"

PASSPHRASE="fixture-passphrase"

encrypt_with_passphrase() {
    # age reads passphrases only from the terminal, so drive it with expect.
    input="$1"; output="$2"; shift 2
    expect <<EXPECT >/dev/null
log_user 0
spawn age --passphrase $* --output $output $input
expect "Enter passphrase*"
send "$PASSPHRASE\r"
expect "Confirm passphrase*"
send "$PASSPHRASE\r"
expect eof
EXPECT
}

printf 'Dummy fixture created with the reference Go age CLI.\nusername: example\npassword: example\n' > interop-plaintext.txt
printf 'Grüße — 日本語 — 🔐 — dummy unicode fixture\n' > unicode-plaintext.txt
printf 'valid prefix \377\376 invalid UTF-8 bytes\n' > not-utf8-plaintext.bin

encrypt_with_passphrase interop-plaintext.txt interop-binary.txt.age
encrypt_with_passphrase interop-plaintext.txt interop-armored.txt.age --armor
encrypt_with_passphrase unicode-plaintext.txt unicode.txt.age
encrypt_with_passphrase not-utf8-plaintext.bin not-utf8.bin.age

age-keygen -o fixture-identity.txt 2>/dev/null
recipient=$(age-keygen -y fixture-identity.txt)
age --recipient "$recipient" --output recipient-encrypted.txt.age interop-plaintext.txt
rm fixture-identity.txt

echo "Fixtures regenerated with $(age --version)."
