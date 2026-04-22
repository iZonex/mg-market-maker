import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright config for UI-side regression tests.
 *
 * The tests assume a live `stand-up.sh` at `../.stand-run/stand.env`
 * — globalSetup spawns `scripts/stand-up.sh` if the env file is
 * missing, and reads the resulting `HTTP_URL` + `ADMIN_TOKEN` +
 * `AGENT_ID` + `DEPLOYMENT_ID` out. Teardown leaves the stand
 * alive for iterative runs; use `scripts/tear-down.sh` manually
 * when done. CI should run tear-down as a separate post-step.
 *
 * The `baseURL` is filled dynamically from stand.env so tests
 * don't hard-code a port.
 */
export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: false,          // shared stand, sequential is safer
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
  globalSetup: './tests/e2e/global-setup.ts',
  timeout: 60_000,
  expect: { timeout: 15_000 },
  use: {
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    // Pass stand state to each test via `process.env` so spec files
    // can read them without re-parsing stand.env.
    ignoreHTTPSErrors: true,
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
})
