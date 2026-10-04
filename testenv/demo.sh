#!/bin/sh
# Run Tern against the local test servers with an isolated XDG tree in
# testenv/.demo (config, data, state, cache), so your real setup is untouched.
#
#   docker compose -f testenv/compose.yaml up -d
#   testenv/seed.py demo
#   testenv/demo.sh [path/to/tern]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root="$here/.demo"
tern=${1:-"$here/../build/frontends/qt/tern"}

mkdir -p "$root/config/tern/profiles/demo/accounts"
cat > "$root/config/tern/profiles/demo/accounts/local.toml" <<'EOF'
name = "Local Dovecot"
email = "demo@example.org"
display_name = "Demo User"
archive = "Archive/{year}"

[compose]
format = "markdown"
signature = "signatures/demo.md"

[imap]
host = "127.0.0.1"
port = 31143
tls = "insecure-plaintext"
username = "demo"
password.command = "echo pass"

[smtp]
host = "127.0.0.1"
port = 1025
tls = "insecure-plaintext"
username = "demo"
password.command = "echo pass"
EOF

mkdir -p "$root/config/tern/signatures"
cat > "$root/config/tern/signatures/demo.md" <<'SIG'
**Demo User** · Tern test account
<https://example.org>
SIG

export XDG_CONFIG_HOME="$root/config" XDG_DATA_HOME="$root/data"
export XDG_STATE_HOME="$root/state" XDG_CACHE_HOME="$root/cache"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$root/runtime}"
exec "$tern" "$@"
