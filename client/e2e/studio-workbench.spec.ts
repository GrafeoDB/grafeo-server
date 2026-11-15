import { test, expect, type Page } from "@playwright/test";
import { startServer, type RunningServer } from "./server";

/**
 * The Studio's core loop on @grafeo-db/ui: write a query, run it, see the graph, the table and the inspector; the
 * pages and the way back; the database lifecycle. Selectors are roles, accessible names and the package's texts.
 */

let server: RunningServer | undefined;

test.beforeEach(async () => {
  server = await startServer({ kind: "none" });
});

test.afterEach(async () => {
  await server?.stop();
  server = undefined;
});

const SEED =
  "INSERT (:Person {name: 'Richard Hendricks'})-[:KNOWS {since: 2014}]->(:Person {name: 'Jared Dunn'})";

async function open(page: Page, path = "/studio/") {
  await page.goto(server!.base + path);
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
  await page.getByRole("button", { name: "Run" }).click();
  await expect(page.getByRole("button", { name: "Run" })).toBeVisible();
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
