#!/bin/sh
# Run inside a network-isolated Linux container. Only synthetic credentials/data.
set -eu
tmp=$(mktemp -d)
pid=
cleanup() {
  if [ -n "$pid" ]; then kill -INT "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM
export SPOTIFY_CLIENT_ID=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
export SPOTIFY_REDIRECT_URI=https://promptlist.invalid/callback
export PROMPTLIST_MODE=hosted
export PROMPTLIST_ALLOWED_USERS=synthetic-owner
export PROMPTLIST_DATA_DIR="$tmp/data"
export PROMPTLIST_PORT=8787
unset LASTFM_API_KEY OPENAI_API_KEY
cd "$tmp"
/release/promptlist > "$tmp/server.log" 2>&1 &
pid=$!
attempt=0
until curl -fsS --max-time 2 -H 'Host: promptlist.invalid' http://127.0.0.1:8787/health > "$tmp/health" 2>/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -gt 20 ]; then echo 'FAIL: Linux startup'; exit 1; fi
  sleep 1
done
test "$(cat "$tmp/health")" = ok
test "$(curl -sS -o /dev/null -w '%{http_code}' -H 'Host: attacker.invalid' http://127.0.0.1:8787/)" = 403
test "$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H 'Host: promptlist.invalid' -H 'Origin: https://attacker.invalid' http://127.0.0.1:8787/api/playback/token)" = 403
curl -fsS -D "$tmp/headers" -o "$tmp/page" -H 'Host: promptlist.invalid' http://127.0.0.1:8787/
grep -qi 'set-cookie:.*HttpOnly.*SameSite=Lax.*Secure' "$tmp/headers"
grep -qi 'strict-transport-security: max-age=31536000' "$tmp/headers"
grep -qi 'cache-control: no-store' "$tmp/headers"
grep -q 'Connect Spotify' "$tmp/page"
test -f "$tmp/data/catalog.sqlite"
echo 'PASS: Linux startup, HTTPS-origin configuration, host/origin denial, Secure cookies, private SQLite and rendered page'
