import { defineConfig, devices } from "@playwright/test";

/**
 * Browser tests of the Studio against a real grafeo-server binary
 * (`GRAFEO_SERVER_BIN`, built with the Studio embedded). Each test starts its
 * own server with the authentication it needs (see e2e/server.ts).
 */
// Every run writes to its own folder, so a later run never clears the traces of a failure. The runner sets the id
// once; the workers, which load this file again, inherit it through the environment.
process.env.GRAFEO_E2E_RUN ??= new Date().toISOString().replace(/[:.]/g, "-");

export default defineConfig({
  testDir: "./e2e",
  outputDir: `test-results/${process.env.GRAFEO_E2E_RUN}`,
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
