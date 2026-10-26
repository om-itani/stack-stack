import { expect, test } from "@playwright/test";
import { signIn } from "./helpers";

test("signs in and shows the dashboard", async ({ page }) => {
  await signIn(page);

  // The shell is the sidebar plus the header controls.
  await expect(page.getByRole("button", { name: "sign out" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Containers", exact: false })).toBeVisible();
  await expect(page.getByRole("button", { name: "Developer", exact: false })).toBeVisible();
});
