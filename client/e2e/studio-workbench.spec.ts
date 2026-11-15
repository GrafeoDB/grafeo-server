import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test, expect, type Page } from "@playwright/test";
import { startServer, type RunningServer } from "./server";

/**
 * The Studio's core loop on @grafeo-db/ui: write a query, run it, see the graph, the table and the inspector; the
 * pages and the way back; the database lifecycle. Selectors are roles, accessible names and the package's texts.
 */

let server: RunningServer | undefined;

test.afterEach(async () => {
  await server?.stop();
  server = undefined;
});

const SEED =
  "INSERT (:Person {name: 'Richard Hendricks'})-[:KNOWS {since: 2014}]->(:Person {name: 'Jared Dunn'})";

/** Opens the Studio, on a server without authentication unless the test started its own. */
async function open(page: Page, path = "/studio/") {
  server ??= await startServer({ kind: "none" });
  await page.goto(server.base + path);
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
}

function editor(page: Page) {
  return page.getByRole("textbox", { name: "Query editor" });
}

/** Replace the active tab's text and run it. */
async function run(page: Page, query: string) {
  const box = editor(page);
  await box.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  await box.pressSequentially(query);
  // The run's answer, then the button back at "Run": the name is exact, since "Run" alone also matches "Running…".
  const answered = queryAnswered(page);
  await runButton(page).click();
  await answered;
  await expect(runButton(page)).toBeVisible();
}

/** The Run button while no query runs (exact: "Running…" contains "Run"). */
function runButton(page: Page) {
  return page.getByRole("button", { name: "Run", exact: true });
}

/** The next answer to a query run. */
function queryAnswered(page: Page) {
  return page.waitForResponse((res) => res.request().method() === "POST" && new URL(res.url()).pathname === "/query");
}

async function seed(page: Page) {
  await run(page, SEED);
  await run(page, "MATCH (a)-[r]->(b) RETURN a, r, b");
  await expect(page.getByText("2 nodes · 1 edge", { exact: false })).toBeVisible();
}

test("runs a query and shows its graph, table and inspector", async ({ page }) => {
  await open(page);
  await seed(page);

  const results = page.getByRole("table", { name: "Results" });
  await expect(results).toBeVisible();
  await expect(results.getByRole("row")).toHaveCount(2); // header and one row

  // A node cell selects its node, and the inspector shows it.
  await results.getByRole("button", { name: /Richard Hendricks/ }).first().click();
  await expect(page.getByRole("complementary", { name: "Inspector" }).getByText("Richard Hendricks").first()).toBeVisible();
});

test("shows a failed query as an error, never as an empty result", async ({ page }) => {
  await open(page);
  await run(page, "MATCH (n RETURN n");
  await expect(page.getByRole("alert").first()).toBeVisible();
  await expect(page.getByRole("table", { name: "Results" })).toHaveCount(0);
});

test("explains a query in the plan view", async ({ page }) => {
  await open(page);
  await run(page, "MATCH (n) RETURN n");
  await page.getByRole("button", { name: "Explain" }).click();
  await expect(page.getByText(/NodeScan/).first()).toBeVisible();
});

test("keeps the workbench across a page and back", async ({ page }) => {
  await open(page);
  await seed(page);
  const studio = page.getByRole("list", { name: "Studio" });

  await studio.getByRole("button", { name: "Databases", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Databases", level: 1 })).toBeVisible();
  await expect(page.getByRole("table", { name: "Databases" })).toBeVisible();

  await studio.getByRole("button", { name: "Query", exact: true }).click();
  await expect(editor(page)).toContainText("MATCH (a)-[r]->(b) RETURN a, r, b");
  await expect(page.getByText("2 nodes · 1 edge", { exact: false })).toBeVisible();
});

test("an example opens in a new tab when the active one holds text", async ({ page }) => {
  await open(page);
  const tabs = page.getByRole("tablist");
  const before = await tabs.getByRole("tab").count();
  await page.getByRole("list", { name: "Examples" }).getByRole("button", { name: "Count nodes" }).click();
  await expect(tabs.getByRole("tab")).toHaveCount(before + 1);
  await expect(editor(page)).toContainText("count(n)");
});

test("saves a query and lists it under Saved queries", async ({ page }) => {
  await open(page);
  await run(page, "MATCH (n) RETURN count(n) AS nodes");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Save query" });
  await dialog.getByLabel("Name").fill("count everything");
  await dialog.getByRole("button", { name: "Save query" }).click();

  await page.getByRole("list", { name: "Studio" }).getByRole("button", { name: "Saved queries", exact: true }).click();
  const saved = page.getByRole("table", { name: "Saved queries" });
  await expect(saved.getByText("count everything")).toBeVisible();

  // Survives a reload: kept in this browser.
  await page.reload();
  await expect(page.getByRole("table", { name: "Saved queries" }).getByText("count everything")).toBeVisible();
});

test("creates a database with a name alone, queries it, and deletes it", async ({ page }) => {
  await open(page, "/studio/databases");
  await page.getByRole("button", { name: "New database" }).click();
  const dialog = page.getByRole("dialog", { name: "New database" });
  // Every other option holds the server's default, so the name alone is enough.
  await dialog.getByText("More options").click();
  await expect(dialog.getByLabel("Memory limit (MB)")).toHaveValue("512");
  await dialog.getByLabel("Name").fill("hooli");
  await dialog.getByRole("button", { name: "Create database" }).click();

  // The new database opens its details and joins the sidebar list.
  await expect(page.getByRole("heading", { name: /hooli/, level: 1 })).toBeVisible();
  const list = page.getByRole("list", { name: "Databases" });
  await expect(list.getByRole("button", { name: "hooli", exact: true })).toBeVisible();

  // Delete asks for the name before it enables.
  await page.getByRole("button", { name: "Delete database" }).click();
  const confirm = page.getByRole("dialog", { name: "Delete hooli?" });
  const destroy = confirm.getByRole("button", { name: "Delete database" });
  await expect(destroy).toBeDisabled();
  await confirm.getByRole("textbox").fill("hooli");
  await destroy.click();

  await expect(page.getByRole("heading", { name: "Databases", level: 1 })).toBeVisible();
  await expect(list.getByRole("button", { name: "hooli", exact: true })).toHaveCount(0);
});

test("remembers the theme across a reload", async ({ page }) => {
  await open(page);
  const theme = () => page.evaluate(() => document.documentElement.dataset.theme);
  expect(await theme()).toBe("dark");
  await page.getByRole("button", { name: /light/i }).first().click();
  expect(await theme()).toBe("light");
  await page.reload();
  expect(await theme()).toBe("light");
});

test("old addresses lead to the new pages", async ({ page }) => {
  await open(page, "/studio/admin/tokens");
  await expect(page).toHaveURL(/\/studio\/tokens$/);
  await open(page, "/studio/admin/backups");
  await expect(page).toHaveURL(/\/studio\/databases$/);
});

test("a server without credentials has no token management to show", async ({ page }) => {
  // The token list's answer first, so the check below is not passed by a list still loading.
  const tokensAnswered = page.waitForResponse((res) => new URL(res.url()).pathname === "/admin/tokens");
  await open(page);
  await tokensAnswered;
  await expect(page.getByRole("list", { name: "Studio" }).getByRole("button", { name: "Databases" })).toBeVisible();
  await expect(page.getByRole("list", { name: "Studio" }).getByRole("button", { name: "API tokens" })).toHaveCount(0);
});

test("creates a token, shows it once, counts it, and revokes it", async ({ browser }) => {
  const dir = mkdtempSync(join(tmpdir(), "grafeo-e2e-"));
  server = await startServer({ kind: "basic", user: "admin", password: "secret" }, ["--data-dir", dir]);
  const context = await browser.newContext({ httpCredentials: { username: "admin", password: "secret" } });
  const page = await context.newPage();
  await open(page);
  const entry = page.getByRole("list", { name: "Studio" }).getByRole("button", { name: "API tokens" });
  await entry.click();
  await expect(page.getByText("No tokens yet. New token creates one and shows it once.")).toBeVisible();

  await page.getByRole("button", { name: "New token" }).click();
  const dialog = page.getByRole("dialog", { name: "New token" });
  await dialog.getByLabel("Name").fill("ci-pipeline");
  await dialog.getByLabel("Role").selectOption("read-write");
  await dialog.getByRole("button", { name: "Create token" }).click();

  const reveal = page.getByRole("region", { name: "New token" });
  await expect(reveal.getByText("Copy the token now. It is shown only this once.")).toBeVisible();
  const table = page.getByRole("table", { name: "API tokens" });
  await expect(table.getByText("ci-pipeline")).toBeVisible();
  await expect(entry).toHaveAccessibleDescription("1");

  await reveal.getByRole("button", { name: "Done" }).click();
  await expect(reveal).toHaveCount(0);

  await table.getByRole("button", { name: "Revoke" }).click();
  await page.getByRole("dialog", { name: "Revoke ci-pipeline?" }).getByRole("button", { name: "Revoke token" }).click();
  await expect(page.getByText("No tokens yet. New token creates one and shows it once.")).toBeVisible();
  await expect(entry).toHaveAccessibleDescription("0");
  await context.close();
});

test("Ctrl+Up and Ctrl+Down step through the history and give the draft back", async ({ page }) => {
  await open(page);
  await run(page, "MATCH (n) RETURN n LIMIT 1");
  await run(page, "MATCH (n) RETURN n LIMIT 2");
  const box = editor(page);
  await box.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  await box.pressSequentially("draft in progress");

  await page.keyboard.press("Control+ArrowUp");
  await expect(box).toContainText("LIMIT 2");
  await page.keyboard.press("Control+ArrowUp");
  await expect(box).toContainText("LIMIT 1");
  await page.keyboard.press("Control+ArrowDown");
  await expect(box).toContainText("LIMIT 2");
  await page.keyboard.press("Control+ArrowDown");
  await expect(box).toContainText("draft in progress");
});

test("a write shows in the sidebar's schema without a reload", async ({ page }) => {
  await open(page);
  const schema = page.getByRole("group", { name: "Schema · default" });
  await expect(schema.getByText("Person")).toHaveCount(0);
  await run(page, SEED);
  await expect(schema.getByText("Person")).toBeVisible();
  await expect(schema.getByText("KNOWS")).toBeVisible();
});

test("the default database cannot be deleted", async ({ page }) => {
  await open(page, "/studio/databases/default");
  await expect(page.getByText("The default database cannot be deleted; the server always keeps it.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Delete database" })).toBeDisabled();
});

test("an answer for the database left behind is not shown under the new one", async ({ page }) => {
  await open(page);
  await page.request.post(`${server!.base}/db`, { data: { name: "hooli" } });
  await page.reload();
  await run(page, SEED);
  await expect(page.getByText("2 nodes · 1 edge", { exact: false })).toHaveCount(0);

  // The next query answers slowly; the database changes while it runs.
  await page.route("**/query", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 1500));
    await route.continue();
  });
  const box = editor(page);
  await box.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  await box.pressSequentially("MATCH (a)-[r]->(b) RETURN a, r, b");
  const answered = queryAnswered(page);
  await runButton(page).click();
  await expect(page.getByRole("button", { name: "Running…" })).toBeVisible();
  await page.getByRole("list", { name: "Databases" }).getByRole("button", { name: "hooli", exact: true }).click();
  await expect(page.getByText("database: hooli")).toBeVisible();
  // The slow answer arrives for the database left behind; the Studio settles without showing it.
  await answered;
  await expect(runButton(page)).toBeVisible();
  await expect(page.getByText("2 nodes · 1 edge", { exact: false })).toHaveCount(0);
  await expect(page.getByRole("table", { name: "Results" })).toHaveCount(0);
});

test("picking a database by keyboard keeps the focus on it", async ({ page }) => {
  await open(page);
  await page.request.post(`${server!.base}/db`, { data: { name: "hooli" } });
  await page.reload();
  const list = page.getByRole("list", { name: "Databases" });
  await list.getByRole("button", { name: "hooli", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("database: hooli")).toBeVisible();
  await expect(list.getByRole("button", { name: "hooli", exact: true })).toBeFocused();
});

/** Marks a DOM node, to tell later whether it is still the same one (nothing remounted). */
async function mark(page: Page, selector: string) {
  await page.locator(selector).first().evaluate((el) => el.setAttribute("data-e2e-mark", "kept"));
}

test("a page visit keeps the graph as it was", async ({ page }) => {
  await open(page);
  await seed(page);
  // The graph view's own element: present with or without WebGL (a browser without it, such as headless Firefox on
  // CI, shows the written note instead of a canvas). A remount would replace it, mark and all.
  await mark(page, ".gu-shell-main .gu-graph");
  const canvases = await page.locator(".gu-shell-main .gu-graph canvas").count();
  if (canvases > 0) await mark(page, ".gu-shell-main .gu-graph canvas");
  const studio = page.getByRole("list", { name: "Studio" });
  await studio.getByRole("button", { name: "Databases", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Databases", level: 1 })).toBeVisible();
  await studio.getByRole("button", { name: "Query", exact: true }).click();
  // The same graph view (and canvas, where there is one): hidden, not drawn and laid out again.
  await expect(page.locator('.gu-shell-main .gu-graph[data-e2e-mark="kept"]')).toHaveCount(1);
  if (canvases > 0) {
    await expect(page.locator('.gu-shell-main .gu-graph canvas[data-e2e-mark="kept"]')).toHaveCount(1);
  }
  await expect(page.getByText("2 nodes · 1 edge", { exact: false })).toBeVisible();
});

test("switching database keeps the workbench mounted", async ({ page }) => {
  await open(page);
  await page.request.post(`${server!.base}/db`, { data: { name: "hooli" } });
  await page.reload();
  await mark(page, ".gu-shell");
  const list = page.getByRole("list", { name: "Databases" });
  await list.getByRole("button", { name: "hooli", exact: true }).click();
  await expect(page.getByText("database: hooli")).toBeVisible();
  await expect(page.locator('.gu-shell[data-e2e-mark="kept"]')).toHaveCount(1);
  await expect(page.getByRole("group", { name: "Schema · hooli" })).toBeVisible();
});

test("a query tab can be renamed, and keeps its name", async ({ page }) => {
  await open(page);
  const tab = page.getByRole("tablist").getByRole("tab").first();
  await tab.dblclick();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type("hooli growth");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("tablist").getByRole("tab", { name: /hooli growth/ })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("tablist").getByRole("tab", { name: /hooli growth/ })).toBeVisible();
});

test("results copy as JSON, as the server sent them, also from the table view", async ({ page, context, browserName }) => {
  await open(page);
  await seed(page);
  const panel = page.locator(".gu-results");
  await expect(panel.getByRole("button", { name: "Copy JSON" })).toBeVisible();
  if (browserName === "chromium") {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await panel.getByRole("button", { name: "Copy JSON" }).click();
    const copied = await page.evaluate(() => navigator.clipboard.readText());
    // The server's own shape, not the graph shape the parts draw.
    expect(copied).toContain("_labels");
    expect(copied).toContain("Richard Hendricks");
  }
  // The main area showing the table hides the panel, and offers the copies itself.
  await page.getByRole("radio", { name: "Table" }).click();
  await expect(page.locator(".st-main-copy").getByRole("button", { name: "Copy JSON" })).toBeVisible();
  await expect(page.locator(".st-main-copy").getByRole("button", { name: "Copy CSV" })).toBeVisible();
});
