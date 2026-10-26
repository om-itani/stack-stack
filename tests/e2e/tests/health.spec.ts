import { expect, test } from "@playwright/test";
import { BASE_URL, openPage, signIn } from "./helpers";

test("configures an HTTP health check for a reachable endpoint", async ({ page, request }) => {
  // The probe target must itself be healthy.
  const health = await request.get("/api/health");
  expect(health.ok()).toBeTruthy();

  await signIn(page);
  await openPage(page, "Alerts");

  await page.getByRole("button", { name: "add health check" }).click();

  const stack = `e2e-stack-${Date.now()}`;
  await page.getByPlaceholder("web").fill(stack);
  await page
    .getByPlaceholder("http://10.0.0.5:8080/health")
    .fill(`${BASE_URL}/api/health`);

  await page.getByRole("button", { name: "save" }).click();

  await expect(page.getByText("saved")).toBeVisible();
  // The saved row rebuilt from the server response keeps the stack name.
  await expect(page.getByPlaceholder("web")).toHaveValue(stack);
});
