#!/usr/bin/env bash
#
# Development seed script for manual UI testing.
#
# Creates six persistent databases populated with distinctive garbage
# data and a series of backups per database, so the Studio admin UI has
# realistic content to exercise the backup/restore flows against. Re-run
# after any UI change that needs a warm environment.
#
# Usage:
#   ./scripts/dev-seed.sh                    # localhost:7474
#   ./scripts/dev-seed.sh http://host:port   # custom URL
#   BACKUPS_PER_DB=5 ./scripts/dev-seed.sh   # more backup history
#
# Environment:
#   GRAFEO_URL       base URL (default http://localhost:7474)
#   BACKUPS_PER_DB   backups to create per database (default 3)
#   BATCH_SIZE       nodes per INSERT batch (default 50)
#   EDGE_PARALLEL    concurrent edge inserts (default 16)
#
# The script does NOT clean up after itself. To reset, stop the server
# and wipe its --data-dir and --backup-dir, then start fresh and re-run.
#
# The target server must be running with both --data-dir and --backup-dir.
#
# Requires: curl, jq

set -euo pipefail

for tool in curl jq; do
  if ! command -v "$tool" &>/dev/null; then
    echo "ERROR: '$tool' is required but not found in PATH" >&2
    exit 1
  fi
done

BASE_URL="${1:-${GRAFEO_URL:-http://localhost:7474}}"
BACKUPS_PER_DB="${BACKUPS_PER_DB:-3}"
BATCH_SIZE="${BATCH_SIZE:-50}"
EDGE_PARALLEL="${EDGE_PARALLEL:-16}"

if [ -t 1 ]; then
  GREEN='\033[0;32m'; CYAN='\033[0;36m'; YELLOW='\033[0;33m'; DIM='\033[2m'; NC='\033[0m'
else
  GREEN=''; CYAN=''; YELLOW=''; DIM=''; NC=''
fi

hdr()  { printf "\n${CYAN}==>${NC} %s\n" "$*"; }
ok()   { printf "    ${GREEN}✓${NC} %s\n" "$*"; }
note() { printf "    ${DIM}%s${NC}\n" "$*"; }
warn() { printf "    ${YELLOW}!${NC} %s\n" "$*"; }

# ── Pre-flight ────────────────────────────────────────────────────

if ! curl -sf -o /dev/null "$BASE_URL/health"; then
  echo "ERROR: server at $BASE_URL is not reachable" >&2
  exit 1
fi

# GET /backups returns [] when --backup-dir is set, 400 otherwise. Non-intrusive probe.
if ! curl -sf -o /dev/null "$BASE_URL/backups"; then
  echo "ERROR: server at $BASE_URL was not started with --backup-dir" >&2
  exit 1
fi

# ── API helpers ───────────────────────────────────────────────────

gql() {
  local db="$1" query="$2"
  local body
  body=$(jq -nc --arg q "$query" --arg db "$db" \
    '{query:$q, language:"gql", database:$db}')
  curl -sf -X POST "$BASE_URL/query" \
    -H 'Content-Type: application/json' -d "$body" > /dev/null
}

create_db() {
  local name="$1"
  local status
  status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE_URL/db" \
    -H 'Content-Type: application/json' \
    -d "{\"name\":\"$name\",\"database_type\":\"Lpg\",\"storage_mode\":\"Persistent\"}")
  case "$status" in
    200|201) ok "created $name" ;;
    *)       echo "ERROR: create $name failed (HTTP $status)" >&2; exit 1 ;;
  esac
}

backup_db() {
  local db="$1"
  local entry
  entry=$(curl -sf -X POST "$BASE_URL/admin/$db/backup")
  local filename size
  filename=$(echo "$entry" | jq -r '.filename')
  size=$(echo "$entry" | jq -r '.size_bytes')
  note "$db: $filename ($size B)"
}

create_index() {
  local db="$1" property="$2"
  curl -sf -X POST "$BASE_URL/admin/$db/index" \
    -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg p "$property" '{type:"property", property:$p}')" > /dev/null
}

# Insert N nodes batched BATCH_SIZE per INSERT. Template uses __I__ as
# the running index placeholder.
seed_nodes() {
  local db="$1" count="$2" template="$3"
  local i=0 batch=""
  while [ "$i" -lt "$count" ]; do
    i=$((i + 1))
    local item="${template//__I__/$i}"
    if [ -z "$batch" ]; then
      batch="$item"
    else
      batch="$batch, $item"
    fi
    if [ "$((i % BATCH_SIZE))" = 0 ] || [ "$i" -eq "$count" ]; then
      gql "$db" "INSERT $batch"
      batch=""
    fi
  done
}

# Fan out MATCH-INSERT edge queries from stdin in parallel.
seed_edges_parallel() {
  local db="$1"
  local pending=0
  while IFS= read -r q; do
    gql "$db" "$q" &
    pending=$((pending + 1))
    if [ "$pending" -ge "$EDGE_PARALLEL" ]; then
      wait
      pending=0
    fi
  done
  wait
}

# ── Seed definitions (10x the original prototype volumes) ─────────

seed_social() {
  hdr "social — people + friendships"
  seed_nodes social 400 "(:Person {name: 'user___I__', age: 25, city: 'city_1'})"
  ok "400 Person nodes"
  for _ in $(seq 1 250); do
    a=$((RANDOM % 400 + 1))
    b=$((RANDOM % 400 + 1))
    printf 'MATCH (a:Person {name: "user_%d"}), (b:Person {name: "user_%d"}) INSERT (a)-[:FRIENDS_WITH]->(b)\n' "$a" "$b"
  done | seed_edges_parallel social
  ok "250 FRIENDS_WITH edges"
}

seed_commerce() {
  hdr "commerce — products, customers, orders"
  seed_nodes commerce 600 "(:Product {sku: 'sku___I__', price: 99, stock: 10})"
  ok "600 Product nodes"
  seed_nodes commerce 300 "(:Customer {id: 'cust___I__', email: 'cust___I__@example.test'})"
  ok "300 Customer nodes"
  for _ in $(seq 1 400); do
    c=$((RANDOM % 300 + 1))
    p=$((RANDOM % 600 + 1))
    printf 'MATCH (c:Customer {id: "cust_%d"}), (p:Product {sku: "sku_%d"}) INSERT (c)-[:BOUGHT {qty: 2}]->(p)\n' "$c" "$p"
  done | seed_edges_parallel commerce
  ok "400 BOUGHT edges"
}

seed_movies() {
  hdr "movies — actors + films"
  seed_nodes movies 200 "(:Movie {title: 'movie___I__', year: 2000})"
  ok "200 Movie nodes"
  seed_nodes movies 150 "(:Actor {name: 'actor___I__', born: 1970})"
  ok "150 Actor nodes"
  for _ in $(seq 1 350); do
    a=$((RANDOM % 150 + 1))
    m=$((RANDOM % 200 + 1))
    printf 'MATCH (a:Actor {name: "actor_%d"}), (m:Movie {title: "movie_%d"}) INSERT (a)-[:ACTED_IN]->(m)\n' "$a" "$m"
  done | seed_edges_parallel movies
  ok "350 ACTED_IN edges"
}

seed_iot() {
  hdr "iot — sensor network"
  seed_nodes iot 1000 "(:Sensor {id: 'sensor___I__', temp: 20, humidity: 50})"
  ok "1000 Sensor nodes"
  seed_nodes iot 100 "(:Gateway {id: 'gw___I__', region: 'region_1'})"
  ok "100 Gateway nodes"
  for _ in $(seq 1 500); do
    g=$((RANDOM % 100 + 1))
    s=$((RANDOM % 1000 + 1))
    printf 'MATCH (g:Gateway {id: "gw_%d"}), (s:Sensor {id: "sensor_%d"}) INSERT (g)-[:MONITORS]->(s)\n' "$g" "$s"
  done | seed_edges_parallel iot
  ok "500 MONITORS edges"
}

seed_knowledge() {
  hdr "knowledge — dense concept graph"
  seed_nodes knowledge 300 "(:Concept {name: 'concept___I__', domain: 'domain_1'})"
  ok "300 Concept nodes"
  for _ in $(seq 1 800); do
    a=$((RANDOM % 300 + 1))
    b=$((RANDOM % 300 + 1))
    printf 'MATCH (a:Concept {name: "concept_%d"}), (b:Concept {name: "concept_%d"}) INSERT (a)-[:RELATED_TO {weight: 0.5}]->(b)\n' "$a" "$b"
  done | seed_edges_parallel knowledge
  ok "800 RELATED_TO edges"
}

seed_tasks() {
  hdr "tasks — assignments"
  seed_nodes tasks 150 "(:Task {title: 'task___I__', done: false, priority: 2})"
  ok "150 Task nodes"
  seed_nodes tasks 100 "(:User {name: 'member___I__'})"
  ok "100 User nodes"
  for _ in $(seq 1 200); do
    u=$((RANDOM % 100 + 1))
    t=$((RANDOM % 150 + 1))
    printf 'MATCH (u:User {name: "member_%d"}), (t:Task {title: "task_%d"}) INSERT (u)-[:ASSIGNED_TO]->(t)\n' "$u" "$t"
  done | seed_edges_parallel tasks
  ok "200 ASSIGNED_TO edges"
}

# ── Indexes ───────────────────────────────────────────────────────

seed_indexes() {
  hdr "creating property indexes"
  create_index social name          && note "social.name"
  create_index commerce sku         && note "commerce.sku"
  create_index commerce id          && note "commerce.id"
  create_index movies title         && note "movies.title"
  create_index movies name          && note "movies.name"
  create_index iot id               && note "iot.id"
  create_index knowledge name       && note "knowledge.name"
  create_index tasks title          && note "tasks.title"
  warn "engine reports index_count=0 for property indexes — known gap"
}

# ── Backup series ─────────────────────────────────────────────────

make_backup_series() {
  hdr "creating $BACKUPS_PER_DB backups per database"
  for db in "${DBS[@]}"; do
    for n in $(seq 1 "$BACKUPS_PER_DB"); do
      if [ "$n" -gt 1 ]; then
        # Mutate lightly so each backup represents a distinct point in time.
        gql "$db" "INSERT (:_SeedMarker {round: $n, at: $(date +%s)})"
      fi
      backup_db "$db"
    done
  done
}

# ── Run ───────────────────────────────────────────────────────────

DBS=(social commerce movies iot knowledge tasks)

hdr "Target: $BASE_URL"
note "BACKUPS_PER_DB=$BACKUPS_PER_DB BATCH_SIZE=$BATCH_SIZE EDGE_PARALLEL=$EDGE_PARALLEL"

# Fail early if any target db already exists — this script expects a clean slate.
existing=$(curl -sf "$BASE_URL/db" | jq -r '.databases[].name' || true)
for db in "${DBS[@]}"; do
  if printf '%s\n' "$existing" | grep -qx "$db"; then
    echo "ERROR: database '$db' already exists on $BASE_URL" >&2
    echo "       This script seeds a clean slate. Reset and re-run:" >&2
    echo "       pkill grafeo-server && rm -rf <data-dir> <backup-dir>" >&2
    exit 1
  fi
done

hdr "Creating databases"
for db in "${DBS[@]}"; do
  create_db "$db"
done

seed_social
seed_commerce
seed_movies
seed_iot
seed_knowledge
seed_tasks

seed_indexes

make_backup_series

hdr "Database summary"
curl -sf "$BASE_URL/db" \
  | jq -r '.databases[] | "  \(.name): \(.node_count)n \(.edge_count)e persistent=\(.persistent)"'

hdr "Backup counts"
for db in "${DBS[@]}"; do
  count=$(curl -sf "$BASE_URL/admin/$db/backups" | jq '. | length')
  printf "  %-12s %s backup(s)\n" "$db:" "$count"
done

hdr "Done. Open $BASE_URL/studio/ → Databases"
