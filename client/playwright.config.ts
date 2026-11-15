import { readdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { defineConfig, devices } from "@playwright/test";

/**
 * Browser tests of the Studio against a real grafeo-server binary
 * (`GRAFEO_SERVER_BIN`, built with the Studio embedded). Each test starts its
 * own server with the authentication it needs (see e2e/server.ts).
 */
// Every run writes to its own folder, so a later run never clears the traces of a failure. The runner sets the id
// once; the workers, which load this file again, inherit it through the environment. `--last-failed` reads the
// folder of the run it repeats: pass that run's id, `GRAFEO_E2E_RUN=<folder> npx playwright test --last-failed`.
const RESULTS = "test-results";
/** Run folders kept, the newest first; older ones are removed when a new run starts. */
const KEEP_RUNS = 5;
if (process.env.GRAFEO_E2E_RUN === undefined) {
  process.env.GRAFEO_E2E_RUN = new Date().toISOString().replace(/[:.]/g, "-");
  pruneRuns();
}

/** Removes all but the newest `KEEP_RUNS - 1` run folders (ids are ISO times, so they sort by age). */
function pruneRuns() {
  let runs: string[];
  try {
    runs = readdirSync(RESULTS, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && /^\d{4}-\d{2}-\d{2}T/.test(entry.name))
      .map((entry) => entry.name)
      .sort();
  } catch {
    return; // no results yet
  }
  for (const old of runs.slice(0, Math.max(0, runs.length - (KEEP_RUNS - 1)))) {
    rmSync(join(RESULTS, old), { recursive: true, force: true });
  }
}

export default defineConfig({
  testDir: "./e2e",
  outputDir: `${RESULTS}/${process.env.GRAFEO_E2E_RUN}`,
  timeout: 30_000,
  // One retry on CI catches a slow runner without hiding a real failure:
  // a test that fails twice fails the job.
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 3 : undefined,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    trace: "retain-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "firefox", use: { ...devices["Desktop Firefox"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
