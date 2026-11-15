import { test, expect, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { networkInterfaces } from "node:os";
import { startServer, type Auth, type RunningServer } from "./server";

/**
 * #90: behind HTTP Basic auth the Studio showed no databases when it was
 * opened anywhere but the server root. The browser logs in with its own Basic
 * prompt and sends the credentials only below the path it was asked on, so a
 * login at /studio/ never reached /db. These tests open the Studio every way
 * a user does, on every authentication mode, and check that it loads its
 * data, or says why it cannot, but never shows an empty Studio.
 *
 * Selectors are roles, accessible names and the exact texts shared with
 * @grafeo-db/ui, so the suite carries over to the rebuilt Studio.
 */

const USER = "admin";
const PASSWORD = "secret";
const TOKEN = "e2e-token";

const NOT_SIGNED_IN = "Not signed in. The server refused this request; sign in to load it.";
const NO_ACCESS = "No access. The server refused this request for this account.";

let server: RunningServer | undefined;

test.afterEach(async () => {
  await server?.stop();
  server = undefined;
});

async function start(auth: Auth): Promise<string> {
  server = await startServer(auth);
  return server.base;
}

/** This machine's first non-loopback IPv4 address, if it has one. */
function lanAddress(): string | undefined {
  for (const addresses of Object.values(networkInterfaces())) {
    for (const a of addresses ?? []) if (a.family === "IPv4" && !a.internal) return a.address;
  }
  return undefined;
}

/** A browser session that answers the login prompt with these credentials. */
async function signedIn(browser: Browser, password = PASSWORD): Promise<BrowserContext> {
  return browser.newContext({ httpCredentials: { username: USER, password } });
}

/** Every API response the page gets, as "METHOD /path STATUS". */
function recordApi(page: Page): string[] {
  const seen: string[] = [];
  page.on("response", (res) => {
    const url = new URL(res.url());
    if (url.pathname.startsWith("/studio") || url.pathname === "/") return;
    seen.push(`${res.request().method()} ${url.pathname} ${res.status()}`);
  });
  return seen;
}

/**
 * The Studio loaded its data: connected, the default database in the
 * sidebar's Databases list (on every page), no refusal.
 */
async function expectStudioLoaded(page: Page) {
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("list", { name: "Databases" }).getByRole("button", { name: "default", exact: true }),
  ).toBeVisible();
  await expect(page.getByText(NOT_SIGNED_IN)).toHaveCount(0);
  await expect(page.getByText(NO_ACCESS)).toHaveCount(0);
}

test.describe("Studio behind Basic auth (#90)", () => {
  for (const entry of ["/", "/studio/", "/studio/databases"]) {
    test(`loads its data when opened at ${entry}`, async ({ browser }) => {
      const base = await start({ kind: "basic", user: USER, password: PASSWORD });
      const context = await signedIn(browser);
      const page = await context.newPage();
      const api = recordApi(page);
      await page.goto(base + entry);
      await expectStudioLoaded(page);
      // The list entry above proves the database list was answered: a refused list
      // shows the refused note instead. (Firefox reports the challenge's 401 to
      // the page but not the silent retry, so response events cannot show it.)
      expect(api.some((r) => r.startsWith("GET /db "))).toBe(true);
      expect(api.filter((r) => r.endsWith(" 403"))).toEqual([]);
      await context.close();
    });
  }

  test("keeps its data after a reload", async ({ browser }) => {
    const base = await start({ kind: "basic", user: USER, password: PASSWORD });
    const context = await signedIn(browser);
    const page = await context.newPage();
    await page.goto(`${base}/studio/`);
    await expectStudioLoaded(page);
    await page.reload();
    await expectStudioLoaded(page);
    await context.close();
  });

  test("loads in a new browser session opened at a bookmark", async ({ browser }) => {
    const base = await start({ kind: "basic", user: USER, password: PASSWORD });
    for (let session = 0; session < 2; session++) {
      const context = await signedIn(browser);
      const page = await context.newPage();
      await page.goto(`${base}/studio/databases`);
      await expectStudioLoaded(page);
      await context.close();
    }
  });

  test("loads at the root after a session that started at /studio/", async ({ browser }) => {
    const base = await start({ kind: "basic", user: USER, password: PASSWORD });
    const context = await signedIn(browser);
    const page = await context.newPage();
    await page.goto(`${base}/studio/`);
    await expectStudioLoaded(page);
    await page.goto(`${base}/`);
    await expectStudioLoaded(page);
    await context.close();
  });

  test("loads on a plain-HTTP address that is not localhost", async ({ browser }) => {
    // Browsers send fetch metadata (Sec-Fetch-Site) only to HTTPS or localhost. Reached by a LAN address over plain
    // HTTP, the Studio's own marker header is what earns the Basic challenge, and the page runs outside a secure
    // context (no crypto.randomUUID).
    const lan = lanAddress();
    test.skip(!lan, "no non-loopback IPv4 address on this machine");
    server = await startServer({ kind: "basic", user: USER, password: PASSWORD }, [], lan!);
    const context = await signedIn(browser);
    const page = await context.newPage();
    await page.goto(`${server.base}/studio/`);
    expect(await page.evaluate(() => window.isSecureContext)).toBe(false);
    await expectStudioLoaded(page);
    // And it writes there: an IP address in Host is never a DNS-rebinding name.
    const status = await page.evaluate(async () => {
      const res = await fetch("/query", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Grafeo-Studio": "1" },
        body: JSON.stringify({ query: "INSERT (:Probe {at: 'lan'})" }),
      });
      return res.status;
    });
    expect(status).toBe(200);
    await context.close();
  });

  test("refuses wrong credentials", async ({ browser }) => {
    const base = await start({ kind: "basic", user: USER, password: PASSWORD });
    const context = await signedIn(browser, "wrong");
    const page = await context.newPage();
    const response = await page.goto(`${base}/studio/`);
    expect(response?.status()).toBe(401);
    await context.close();
  });

  test("refuses a cancelled login", async ({ browser }) => {
    const base = await start({ kind: "basic", user: USER, password: PASSWORD });
    const context = await browser.newContext();
    const page = await context.newPage();
    const response = await page.goto(`${base}/studio/`);
    expect(response?.status()).toBe(401);
    await context.close();
  });

  test("loads on a server with Basic auth and a token", async ({ browser }) => {
    const base = await start({ kind: "basic+token", user: USER, password: PASSWORD, token: TOKEN });
    const context = await signedIn(browser);
    const page = await context.newPage();
    await page.goto(`${base}/studio/`);
    await expectStudioLoaded(page);
    await context.close();
  });
});

test.describe("Studio on other authentication modes", () => {
  test("loads without authentication", async ({ page }) => {
    const base = await start({ kind: "none" });
    const response = await page.goto(`${base}/studio/`);
    // No other site may frame the Studio (it would run with the browser's login under another page).
    expect(response?.headers()["x-frame-options"]).toBe("DENY");
    expect(response?.headers()["content-security-policy"]).toBe("frame-ancestors 'none'");
    await expectStudioLoaded(page);
  });

  test("a token-only server refuses the Studio page", async ({ browser }) => {
    // The Studio signs in with Basic credentials only; a sign-in for
    // token-only servers comes with the rebuilt Studio. Until then the page
    // is refused rather than shown empty.
    const base = await start({ kind: "token", token: TOKEN });
    const context = await signedIn(browser);
    const page = await context.newPage();
    const response = await page.goto(`${base}/studio/`);
    expect(response?.status()).toBe(401);
    // No Basic prompt: a user name and password could never get through.
    expect(response?.headers()["www-authenticate"]).toBeUndefined();
    await context.close();
  });
});

test.describe("a refused request is said, never shown as empty", () => {
  for (const [status, text] of [
    [401, NOT_SIGNED_IN],
    [403, NO_ACCESS],
  ] as const) {
    test(`a ${status} on the database list`, async ({ page }) => {
      const base = await start({ kind: "none" });
      await page.route("**/db", (route) =>
        route.fulfill({
          status,
          contentType: "application/json",
          body: JSON.stringify({ error: status === 401 ? "unauthorized" : "forbidden", detail: null }),
        }),
      );

      // The workbench sidebar.
      await page.goto(`${base}/studio/`);
      await expect(page.getByRole("status").filter({ hasText: text })).toBeVisible();
      await expect(page.getByText("No databases")).toHaveCount(0);

      // The databases page: its stats and its cards.
      await page.goto(`${base}/studio/databases`);
      await expect(page.getByRole("status").filter({ hasText: text })).not.toHaveCount(0);
      await expect(page.getByText("No databases yet.")).toHaveCount(0);
      // The stats keep the public server facts but show no database totals.
      await expect(page.getByText("Uptime", { exact: true })).toBeVisible();
      await expect(page.getByText("Nodes", { exact: true })).toHaveCount(0);
    });
  }
});
