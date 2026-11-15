import { defineConfig, devices } from "@playwright/test";

/**
 * Browser tests of the Studio against a real grafeo-server binary
 * (`GRAFEO_SERVER_BIN`, built with the Studio embedded). Each test starts its
 * own server with the authentication it needs (see e2e/server.ts).
 */
export default defineConfig({
  testDir: "./e2e",
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
