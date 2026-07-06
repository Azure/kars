#!/usr/bin/env bash
# kars git credential helper (design note §14 — keyless git write).
#
# git invokes this with a verb ("get"/"store"/"erase") and the credential query
# on stdin. On "get" we fetch a SHORT-LIVED token from the kars inference router
# (which holds the GitHub App private key or an operator-provided PAT — never the
# agent) and hand it to git as the HTTPS password. The agent never persists a
# credential; the token expires within the hour and is scoped to the repos the
# operator configured (the App installation / fine-grained PAT), so the blast
# radius is bounded even though git (UID 1000) uses it.
#
# Fail-closed: if the router has no write credential configured, /v1/github-token
# returns 404 and we emit nothing → git falls back to anonymous (public read).
set -uo pipefail

op="${1:-}"
# Dual purpose:
#   git-credential-kars get     → git credential protocol (username/password)
#   git-credential-kars token   → print just the raw token (for GH_TOKEN=$(...))
case "$op" in
  get) : ;;
  token) : ;;
  # "store"/"erase" and anything else are no-ops (nothing is persisted).
  *) exit 0 ;;
esac

# For the git "get" verb, only mint for GitHub hosts. Read the query git passes
# on stdin. The "token" mode skips host filtering (the caller knows it wants gh).
host=""
if [ "$op" = "get" ]; then
  while IFS='=' read -r key val; do
    [ -z "$key" ] && break
    [ "$key" = "host" ] && host="$val"
  done
  case "$host" in
    github.com|*.github.com|"") : ;;
    *) exit 0 ;;
  esac
fi

ROUTER="${KARS_ROUTER_URL:-http://127.0.0.1:8443}"
ADMIN_TOKEN="$(cat /tmp/.agt-admin-token 2>/dev/null || echo)"
[ -n "$ADMIN_TOKEN" ] || exit 0

resp="$(curl -s --max-time 15 -H "Authorization: Bearer ${ADMIN_TOKEN}" \
  "${ROUTER}/v1/github-token" 2>/dev/null || echo)"
[ -n "$resp" ] || exit 0

token="$(printf '%s' "$resp" | node -e '
let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{
  try{const t=JSON.parse(s).token;process.stdout.write(t?String(t):"");}catch(e){process.stdout.write("");}
});' 2>/dev/null || echo)"
[ -n "$token" ] || exit 0

if [ "$op" = "token" ]; then
  printf '%s' "$token"
  exit 0
fi

echo "username=x-access-token"
echo "password=${token}"
