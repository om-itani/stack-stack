import { defineConfig, devices } from "@playwright/test";

/**
 * End-to-end configuration.
 *
 * The suite talks to an already-running stack-stack instance. CI boots the
 * release binary first (see `.github/workflows/ci.yml`); locally, start it and
 * point `STACK_STACK_URL` at it (the default is the standard listen address).
 */
const baseURL = process.env.STACK_STACK_URL ?? "http://127.0.0.1:8553";

export default defineConfig({
  testDir: "./tests",
  timeout: 45_000,
  expect: { timeout: 10_000 },
  // The backend is a single shared instance; run serially so a test that
  // changes settings cannot race another test's assertions.
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  forbidOnly: Boolean(process.env.CI),
  reporter: process.env.CI
    ? [["list"], ["github"], ["html", { open: "never" }]]
    : [["list"]],
  use: {
    baseURL,
    headless: true,
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
