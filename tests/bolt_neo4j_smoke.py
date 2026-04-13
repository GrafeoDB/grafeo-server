"""Smoke test: Neo4j Python driver 5.x against Grafeo BoltR.

Validates the three Bolt fixes in 0.5.38:
  1. Bookmark propagation from COMMIT
  2. Query result type detection (r vs w)
  3. is_mutation_query() heuristic

Requires: neo4j Python driver 5.x (`pip install neo4j`)
Expects:  grafeo-server running on bolt://localhost:7687
"""

import sys

# The Neo4j Python driver rejects non-Neo4j server agents. Grafeo reports
# "GrafeoDB/x.y.z" which triggers UnsupportedServerProduct. Patch the check
# in all modules that import it before the driver uses them.
import neo4j._sync.io._common
import neo4j._sync.io._bolt5

_noop = lambda agent: None  # noqa: E731
neo4j._sync.io._common.check_supported_server_product = _noop
neo4j._sync.io._bolt5.check_supported_server_product = _noop

import neo4j  # noqa: E402

BOLT_URI = "bolt://localhost:7687"


def main():
    driver = neo4j.GraphDatabase.driver(BOLT_URI, auth=None)
    failures = []

    try:
        # --- Test 1: Basic connectivity + read query ---
        print("[1] Read query via auto-commit...")
        with driver.session(database="default") as session:
            result = session.run("MATCH (n) RETURN count(n) AS cnt")
            record = result.single()
            cnt = record["cnt"]
            summary = result.consume()
            print(f"    count = {cnt}, query_type = {summary.query_type}")
            if summary.query_type != "r":
                failures.append(
                    f"Test 1: expected query_type 'r', got '{summary.query_type}'"
                )

        # --- Test 2: Write transaction + bookmark ---
        print("[2] Write transaction with bookmark propagation...")
        with driver.session(database="default") as session:
            # Explicit write transaction
            def create_node(tx):
                result = tx.run(
                    "CREATE (n:SmokeTest {ts: $ts}) RETURN n",
                    ts=1234,
                )
                record = result.single()
                summary = result.consume()
                return summary

            summary = session.execute_write(create_node)
            bookmark = session.last_bookmarks()

            print(f"    query_type = {summary.query_type}")
            print(f"    bookmarks  = {bookmark}")

            if summary.query_type != "w":
                failures.append(
                    f"Test 2: expected query_type 'w', got '{summary.query_type}'"
                )

            bm_values = list(bookmark.raw_values) if hasattr(bookmark, 'raw_values') else []
            if not bm_values:
                failures.append("Test 2: no bookmark returned from write transaction")
            else:
                bm = bm_values[0]
                if not bm.startswith("grafeo:tx:"):
                    failures.append(
                        f"Test 2: unexpected bookmark format: '{bm}'"
                    )
                else:
                    print(f"    bookmark OK: {bm}")

        # --- Test 3: Read-after-write with bookmark ---
        print("[3] Read-after-write using bookmark...")
        with driver.session(
            database="default", bookmarks=bookmark
        ) as session:
            result = session.run(
                "MATCH (n:SmokeTest {ts: 1234}) RETURN n.ts AS ts"
            )
            record = result.single()
            summary = result.consume()
            if record is None:
                failures.append("Test 3: node not found after write")
            else:
                print(f"    found node: ts = {record['ts']}")
                if record["ts"] != 1234:
                    failures.append(
                        f"Test 3: expected ts=1234, got {record['ts']}"
                    )
            if summary.query_type != "r":
                failures.append(
                    f"Test 3: expected query_type 'r', got '{summary.query_type}'"
                )

        # --- Test 4: DELETE is detected as write ---
        print("[4] DELETE detected as write...")
        with driver.session(database="default") as session:
            result = session.run(
                "MATCH (n:SmokeTest) DELETE n"
            )
            summary = result.consume()
            print(f"    query_type = {summary.query_type}")
            if summary.query_type != "w":
                failures.append(
                    f"Test 4: expected query_type 'w', got '{summary.query_type}'"
                )

    finally:
        driver.close()

    # --- Report ---
    print()
    if failures:
        print(f"FAILED ({len(failures)} issues):")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    else:
        print("ALL 4 TESTS PASSED")
        sys.exit(0)


if __name__ == "__main__":
    main()
