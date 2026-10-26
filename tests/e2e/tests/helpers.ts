import { expect, type Page } from "@playwright/test";

/**
 * Shared helpers for the E2E suite.
 *
 * The admin password is fixed per run so every test can sign in against the
 * same instance; the very first test creates it via the first-run setup screen.
 */
export const PASSWORD = process.env.STACK_STACK_PASSWORD ?? "e2e-test-password-123";

/** Base URL, matching the Playwright config. */
export const BASE_URL = process.env.STACK_STACK_URL ?? "http://127.0.0.1:8553";

/**
 * Bring the browser to the authenticated shell, creating the admin password on
 * first run and signing in otherwise.
 */
export async function signIn(page: Page): Promise<void> {
  await page.goto("/");
  await page.waitForLoadState("networkidle");

  const createPassword = page.getByRole("button", { name: "Create password" });
  if (await createPassword.isVisible().catch(() => false)) {
    const fields = page.locator('input[type="password"]');
    await fields.nth(0).fill(PASSWORD);
    await fields.nth(1).fill(PASSWORD);
    await createPassword.click();
  } else {
    const signInButton = page.getByRole("button", { name: "Sign in" });
    if (await signInButton.isVisible().catch(() => false)) {
      await page.locator('input[type="password"]').first().fill(PASSWORD);
      await signInButton.click();
    }
  }

  // The shell is up once the header sign-out control is present.
  await expect(page.getByRole("button", { name: "sign out" })).toBeVisible();
}

/** Click a sidebar navigation entry by its label. */
export async function openPage(page: Page, label: string): Promise<void> {
  await page.getByRole("button", { name: label, exact: false }).first().click();
}
