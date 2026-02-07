#!/usr/bin/env bash
#
# End-to-end API test for Grafeo Server.
#
# Usage:
#   ./scripts/e2e-test.sh                 # test against localhost:7474
#   ./scripts/e2e-test.sh http://host:port # test against custom URL
#   GRAFEO_URL=http://host:port ./scripts/e2e-test.sh
#
# The script starts its own server when no URL is provided and the
# GRAFEO_E2E_NO_SERVER env var is unset.

set -euo pipefail

# Ensure required tools are available
for tool in curl jq; do
  if ! command -v "$tool" &>/dev/null; then
    echo "ERROR: '$tool' is required but not found in PATH"
    exit 1
  fi
done

BASE_URL="${1:-${GRAFEO_URL:-http://localhost:7474}}"
PASS=0
FAIL=0
SERVER_PID=""

# ── Colours (disabled when not a tty) ────────────────────────────
if [ -t 1 ]; then
  GREEN='\033[0;32m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
else
  GREEN=''; RED=''; CYAN=''; NC=''
fi

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# ── Helpers ──────────────────────────────────────────────────────

assert_status() {
  local label="$1" method="$2" path="$3" expected="$4"
  shift 4
  local status
  status=$(curl -s -o /dev/null -w "%{http_code}" -X "$method" "$BASE_URL$path" "$@")
  if [ "$status" = "$expected" ]; then
    printf "  ${GREEN}PASS${NC} %s (HTTP %s)\n" "$label" "$status"
    PASS=$((PASS + 1))
  else
    printf "  ${RED}FAIL${NC} %s (expected %s, got %s)\n" "$label" "$expected" "$status"
    FAIL=$((FAIL + 1))
  fi
}

assert_json() {
  local label="$1" method="$2" path="$3" jq_expr="$4"
  shift 4
  local body
  body=$(curl -s -X "$method" "$BASE_URL$path" "$@")
  if echo "$body" | jq -e "$jq_expr" > /dev/null 2>&1; then
    printf "  ${GREEN}PASS${NC} %s\n" "$label"
    PASS=$((PASS + 1))
  else
    printf "  ${RED}FAIL${NC} %s\n" "$label"
    printf "       expr: %s\n" "$jq_expr"
    printf "       body: %s\n" "$body"
    FAIL=$((FAIL + 1))
  fi
}

post_json() {
  local path="$1" data="$2"
  shift 2
  curl -s -X POST "$BASE_URL$path" -H "Content-Type: application/json" -d "$data" "$@"
}

# ── Auto-start server if needed ──────────────────────────────────
if [ -z "${GRAFEO_E2E_NO_SERVER:-}" ]; then
  if ! curl -s -o /dev/null "$BASE_URL/health" 2>/dev/null; then
    printf "${CYAN}Starting server...${NC}\n"
    if [ -f "target/release/grafeo-server" ]; then
      BINARY="target/release/grafeo-server"
    elif [ -f "target/debug/grafeo-server" ]; then
      BINARY="target/debug/grafeo-server"
    elif [ -f "target/release/grafeo-server.exe" ]; then
      BINARY="target/release/grafeo-server.exe"
    elif [ -f "target/debug/grafeo-server.exe" ]; then
      BINARY="target/debug/grafeo-server.exe"
    else
      echo "No server binary found — build first with 'cargo build'"
      exit 1
    fi
    "$BINARY" &
    SERVER_PID=$!
    # Wait for server to be ready (up to 15s)
    for i in $(seq 1 30); do
      if curl -s -o /dev/null "$BASE_URL/health" 2>/dev/null; then
        break
      fi
      sleep 0.5
    done
    if ! curl -s -o /dev/null "$BASE_URL/health" 2>/dev/null; then
      echo "Server failed to start"
      exit 1
    fi
    printf "${CYAN}Server ready at %s${NC}\n\n" "$BASE_URL"
  fi
fi

# ═══════════════════════════════════════════════════════════════════
# Tests
# ═══════════════════════════════════════════════════════════════════

printf "${CYAN}=== Health ===${NC}\n"
assert_status "GET /health" GET "/health" 200
assert_json "health fields" GET "/health" \
  '.status == "ok" and .version != null and .engine_version != null and (.uptime_seconds >= 0)'

printf "\n${CYAN}=== Database Management ===${NC}\n"
assert_json "list has default" GET "/db" \
  '.databases | length >= 1 and any(.name == "default")'

# Create database
body=$(post_json "/db" '{"name":"e2e_test"}')
if echo "$body" | jq -e '.name == "e2e_test"' > /dev/null 2>&1; then
  printf "  ${GREEN}PASS${NC} create database e2e_test\n"; PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} create database e2e_test: %s\n" "$body"; FAIL=$((FAIL + 1))
fi

assert_status "duplicate create returns 409" POST "/db" 409 \
  -H "Content-Type: application/json" -d '{"name":"e2e_test"}'

assert_json "list shows e2e_test" GET "/db" \
  '.databases | any(.name == "e2e_test")'

# Database info / stats / schema
assert_status "GET /db/e2e_test" GET "/db/e2e_test" 200
assert_json "db info has fields" GET "/db/e2e_test" \
  '.name == "e2e_test" and (.node_count >= 0) and (.version != null)'

assert_status "GET /db/e2e_test/stats" GET "/db/e2e_test/stats" 200
assert_json "stats has fields" GET "/db/e2e_test/stats" \
  '.name == "e2e_test" and (.memory_bytes >= 0)'

assert_status "GET /db/e2e_test/schema" GET "/db/e2e_test/schema" 200
assert_json "schema has fields" GET "/db/e2e_test/schema" \
  '.name == "e2e_test" and (.labels | type == "array")'

# Cannot delete default
assert_status "cannot delete default" DELETE "/db/default" 400

printf "\n${CYAN}=== Queries (GQL) ===${NC}\n"
# Insert into default
post_json "/query" '{"query":"INSERT (:Person {name: '\''Alice'\'', age: 30})"}' > /dev/null
assert_json "match person" POST "/query" \
  '.rows | length == 1 and .[0][0] == "Alice"' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (p:Person) RETURN p.name"}'

# Insert into e2e_test — should be isolated
post_json "/query" '{"query":"INSERT (:Widget {color: '\''red'\''})","database":"e2e_test"}' > /dev/null
assert_json "widget in e2e_test" POST "/query" \
  '.rows | length == 1 and .[0][0] == "red"' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (w:Widget) RETURN w.color","database":"e2e_test"}'

assert_json "no widget in default" POST "/query" \
  '.rows | length == 0' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (w:Widget) RETURN w.color"}'

assert_json "no person in e2e_test" POST "/query" \
  '.rows | length == 0' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (p:Person) RETURN p.name","database":"e2e_test"}'

printf "\n${CYAN}=== Cypher ===${NC}\n"
post_json "/cypher" '{"query":"CREATE (m:Movie {title: '\''The Matrix'\''}) RETURN m.title"}' > /dev/null
assert_json "cypher match" POST "/cypher" \
  '.rows | length >= 1' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (m:Movie) RETURN m.title"}'

printf "\n${CYAN}=== GraphQL ===${NC}\n"
assert_json "graphql query" POST "/graphql" \
  '.columns | length >= 1' \
  -H "Content-Type: application/json" \
  -d '{"query":"{ Person { name age } }"}'

printf "\n${CYAN}=== Gremlin ===${NC}\n"
assert_json "gremlin query" POST "/gremlin" \
  '.columns | length >= 1' \
  -H "Content-Type: application/json" \
  -d "{\"query\":\"g.V().hasLabel('Person').values('name')\"}"

printf "\n${CYAN}=== SPARQL ===${NC}\n"
post_json "/sparql" '{"query":"PREFIX ex: <http://example.org/> INSERT DATA { ex:bob ex:knows ex:alice }"}' > /dev/null
assert_json "sparql select" POST "/sparql" \
  '.columns | length >= 1' \
  -H "Content-Type: application/json" \
  -d '{"query":"SELECT ?s WHERE { ?s ?p ?o } LIMIT 5"}'

printf "\n${CYAN}=== Transactions ===${NC}\n"
# Begin on default
session_id=$(post_json "/tx/begin" '{}' | jq -r '.session_id')
if [ -n "$session_id" ] && [ "$session_id" != "null" ]; then
  printf "  ${GREEN}PASS${NC} tx begin (session: %s)\n" "${session_id:0:8}..."
  PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} tx begin\n"; FAIL=$((FAIL + 1))
fi

# Query within tx
assert_json "tx query" POST "/tx/query" \
  '.columns | length >= 1' \
  -H "Content-Type: application/json" \
  -H "X-Session-Id: $session_id" \
  -d '{"query":"CREATE (t:TxTest {val: 42}) RETURN t.val"}'

# Commit
assert_status "tx commit" POST "/tx/commit" 200 \
  -H "X-Session-Id: $session_id"

# Double commit fails
assert_status "double commit = 404" POST "/tx/commit" 404 \
  -H "X-Session-Id: $session_id"

# Begin on specific database
session_id2=$(post_json "/tx/begin" '{"database":"e2e_test"}' | jq -r '.session_id')
if [ -n "$session_id2" ] && [ "$session_id2" != "null" ]; then
  printf "  ${GREEN}PASS${NC} tx begin on e2e_test\n"
  PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} tx begin on e2e_test\n"; FAIL=$((FAIL + 1))
fi

# Rollback
assert_status "tx rollback" POST "/tx/rollback" 200 \
  -H "X-Session-Id: $session_id2"

# Missing header
assert_status "tx query without header = 400" POST "/tx/query" 400 \
  -H "Content-Type: application/json" -d '{"query":"RETURN 1"}'

# Bad session
assert_status "tx query bad session = 404" POST "/tx/query" 404 \
  -H "Content-Type: application/json" \
  -H "X-Session-Id: nonexistent" \
  -d '{"query":"RETURN 1"}'

printf "\n${CYAN}=== Error Handling ===${NC}\n"
assert_status "bad syntax = 400" POST "/query" 400 \
  -H "Content-Type: application/json" -d '{"query":"NOT VALID %%%"}'

assert_status "unknown db = 404" POST "/query" 404 \
  -H "Content-Type: application/json" -d '{"query":"RETURN 1","database":"nonexistent"}'

assert_json "error body format" POST "/query" \
  '.error == "bad_request" and .detail != null' \
  -H "Content-Type: application/json" -d '{"query":"NOT VALID %%%"}'

printf "\n${CYAN}=== OpenAPI ===${NC}\n"
assert_status "GET /api/openapi.json" GET "/api/openapi.json" 200
assert_json "openapi has db paths" GET "/api/openapi.json" \
  '.paths | has("/db") and has("/db/{name}") and has("/db/{name}/stats") and has("/db/{name}/schema")'
assert_json "openapi has query paths" GET "/api/openapi.json" \
  '.paths | has("/query") and has("/cypher") and has("/graphql") and has("/gremlin") and has("/sparql")'

printf "\n${CYAN}=== UI ===${NC}\n"
assert_status "GET / redirects" GET "/" 308
assert_status "GET /studio/" GET "/studio/" 200

printf "\n${CYAN}=== Request ID ===${NC}\n"
rid=$(curl -s -D - -o /dev/null "$BASE_URL/health" | grep -i "^x-request-id:" | head -1 | tr -d '\r' | awk '{print $2}')
if [ -n "$rid" ]; then
  printf "  ${GREEN}PASS${NC} x-request-id generated (%s)\n" "$rid"
  PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} x-request-id missing\n"; FAIL=$((FAIL + 1))
fi

rid2=$(curl -s -D - -o /dev/null -H "X-Request-Id: my-id-123" "$BASE_URL/health" \
  | grep -i "^x-request-id:" | head -1 | tr -d '\r' | awk '{print $2}')
if [ "$rid2" = "my-id-123" ]; then
  printf "  ${GREEN}PASS${NC} x-request-id preserved\n"
  PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} x-request-id not preserved (got: %s)\n" "$rid2"
  FAIL=$((FAIL + 1))
fi

printf "\n${CYAN}=== Compression ===${NC}\n"
# Request gzip encoding and verify server honours it
encoding=$(curl -s -D - -o /dev/null -H "Accept-Encoding: gzip" "$BASE_URL/health" \
  | grep -i "^content-encoding:" | head -1 | tr -d '\r' | awk '{print $2}')
if [ "$encoding" = "gzip" ]; then
  printf "  ${GREEN}PASS${NC} gzip compression returned\n"; PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} expected content-encoding: gzip, got: %s\n" "$encoding"; FAIL=$((FAIL + 1))
fi

printf "\n${CYAN}=== Metrics ===${NC}\n"
assert_status "GET /metrics" GET "/metrics" 200

metrics_body=$(curl -s "$BASE_URL/metrics")
for metric in grafeo_databases_total grafeo_uptime_seconds grafeo_active_sessions_total grafeo_queries_total; do
  if echo "$metrics_body" | grep -q "$metric"; then
    printf "  ${GREEN}PASS${NC} metrics contains %s\n" "$metric"; PASS=$((PASS + 1))
  else
    printf "  ${RED}FAIL${NC} metrics missing %s\n" "$metric"; FAIL=$((FAIL + 1))
  fi
done

ct=$(curl -s -D - -o /dev/null "$BASE_URL/metrics" | grep -i "^content-type:" | head -1 | tr -d '\r')
if echo "$ct" | grep -q "text/plain"; then
  printf "  ${GREEN}PASS${NC} metrics content-type is text/plain\n"; PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${NC} metrics content-type: %s\n" "$ct"; FAIL=$((FAIL + 1))
fi

printf "\n${CYAN}=== Query Timeout ===${NC}\n"
assert_json "timeout_ms succeeds" POST "/query" \
  '.columns | length >= 1' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (n) RETURN count(n)","timeout_ms":60000}'

assert_json "timeout_ms=0 disables" POST "/query" \
  '.columns | length >= 1' \
  -H "Content-Type: application/json" \
  -d '{"query":"MATCH (n) RETURN count(n)","timeout_ms":0}'

printf "\n${CYAN}=== Cleanup ===${NC}\n"
assert_status "delete e2e_test db" DELETE "/db/e2e_test" 200
assert_json "e2e_test gone" GET "/db" \
  '.databases | all(.name != "e2e_test")'

# ═══════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════

TOTAL=$((PASS + FAIL))
printf "\n══════════════════════════════════════\n"
if [ "$FAIL" -eq 0 ]; then
  printf "${GREEN}ALL %d TESTS PASSED${NC}\n" "$TOTAL"
else
  printf "${RED}%d/%d FAILED${NC}\n" "$FAIL" "$TOTAL"
fi
printf "══════════════════════════════════════\n"

exit "$FAIL"
