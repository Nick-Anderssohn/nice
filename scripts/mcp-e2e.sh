#!/usr/bin/env bash
#
# End-to-end smoke test for Nice's MCP server. Simulates what a Claude
# process running inside a Nice tab does: handshake → list tools →
# list tabs → verify the response shape.
#
# Tab creation is not covered here — it happens off the MCP surface,
# via the Main Terminal's shadowed `claude()` function talking to
# Nice's control socket. This script only asserts the three MCP tools
# still present are wired up correctly.
#
# Requires: Nice.app running (to have the server bound on 127.0.0.1:7420).
# Dependencies: curl, jq.
#
# Exit codes:
#   0  all checks passed
#   1  prereq missing (jq, not running, etc.)
#   2  a check failed (response shape mismatch)

set -euo pipefail

URL="http://127.0.0.1:7420/mcp"

log()  { printf '[e2e] %s\n' "$*"; }
fail() { printf '[e2e] FAIL: %s\n' "$*" >&2; exit 2; }
need() { command -v "$1" >/dev/null 2>&1 || { printf '[e2e] missing dep: %s\n' "$1" >&2; exit 1; }; }

need curl
need jq

# ── 0. server reachable? ──────────────────────────────────────────────
if ! nc -z 127.0.0.1 7420 2>/dev/null; then
    printf '[e2e] FAIL: Nice MCP server not bound on 127.0.0.1:7420.\n' >&2
    printf '       Launch Nice.app and try again.\n' >&2
    exit 1
fi
log "server reachable on :7420"

# ── 1. initialize handshake ───────────────────────────────────────────
INIT_HEADERS=$(mktemp)
trap 'rm -f "$INIT_HEADERS"' EXIT

INIT_BODY='{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "2024-11-05",
    "capabilities": {},
    "clientInfo": { "name": "mcp-e2e.sh", "version": "1.0" }
  }
}'

INIT_RESP=$(curl -sS -D "$INIT_HEADERS" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    --data "$INIT_BODY" \
    "$URL")

SESSION_ID=$(tr -d '\r' < "$INIT_HEADERS" \
    | awk -F': ' 'tolower($1)=="mcp-session-id"{print $2; exit}')

[[ -n "$SESSION_ID" ]] || fail "no Mcp-Session-Id header in initialize response"
log "initialized — session=$SESSION_ID"

# Required follow-up per the spec — some transports need it before tools/*.
curl -sS -o /dev/null \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H "Mcp-Session-Id: $SESSION_ID" \
    --data '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    "$URL"

# Helper: `call METHOD JSON-PARAMS`. Wraps a JSON-RPC call to $URL and
# prints the JSON body. The MCP transport here always answers with SSE
# (multiple `id: N` frames, some with empty `data:` lines, the real
# payload landing on one `data: {...}` line). We grab the last non-empty
# `data:` line — that's always the actual JSON-RPC response.
call() {
    local method="$1" params="$2"
    local raw
    raw=$(curl -sS \
        -H 'Content-Type: application/json' \
        -H 'Accept: application/json, text/event-stream' \
        -H "Mcp-Session-Id: $SESSION_ID" \
        --data "$(jq -nc --arg m "$method" --argjson p "$params" \
            '{jsonrpc:"2.0", id:($p.id // 2), method:$m, params:($p|del(.id))}')" \
        "$URL")
    printf '%s' "$raw" \
        | grep -E '^data: \{' \
        | tail -n1 \
        | sed 's/^data: //'
}

# ── 2. tools/list advertises the three nice.* tools ──────────────────
TOOLS_JSON=$(call tools/list '{"id":2}')
TOOLS=$(jq -r '.result.tools[].name' <<<"$TOOLS_JSON" | sort | tr '\n' ',' | sed 's/,$//')
EXPECTED="nice.run,nice.tab.list,nice.tab.switch"
[[ "$TOOLS" == "$EXPECTED" ]] || fail "tools/list mismatch: got [$TOOLS], want [$EXPECTED]"
log "tools/list ok: $TOOLS"

# ── 3. nice.tab.list returns a coherent JSON array ────────────────────
LIST_RESP=$(call tools/call "$(jq -nc '{id:3, name:"nice.tab.list", arguments:{}}')")
TABS=$(jq -r '.result.content[0].text | fromjson' <<<"$LIST_RESP")
# Must parse as an array (length is an Int). An error from jq here means
# the server didn't wrap the response correctly.
COUNT=$(jq 'length' <<<"$TABS")
[[ "$COUNT" =~ ^[0-9]+$ ]] \
    || fail "nice.tab.list did not return a JSON array — raw: $LIST_RESP"
log "nice.tab.list ok — count=$COUNT"
log "all checks passed ✓"
