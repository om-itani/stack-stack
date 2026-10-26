import { expect, test } from "@playwright/test";
import { openPage, signIn } from "./helpers";

test("creates and runs a scheduled task", async ({ page }) => {
  await signIn(page);
  await openPage(page, "Tasks");

  const name = `e2e task ${Date.now()}`;
  await page.getByPlaceholder("nightly cleanup").fill(name);
  await page.locator('textarea[placeholder="docker system prune -f"]').fill('echo "hello"');
  await page.getByRole("button", { name: "save task" }).click();

  // The new task is listed with its command.
  const row = page.locator("div", { hasText: name }).first();
  await expect(row).toBeVisible();
  await expect(page.getByText("saved")).toBeVisible();

  // Run it and confirm the captured output. The command box also contains the
  // word "hello", so match the output box by its exact single-line content.
  await page.getByRole("button", { name: "run now" }).first().click();
  await expect(page.locator("pre", { hasText: /^hello\s*$/ })).toBeVisible({
    timeout: 20_000,
  });
});
