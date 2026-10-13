import { defineConfig, devices } from '@playwright/test'

// End-to-end tests start the real `ai-orchestra web` against fake provider CLIs (see e2e/harness.ts):
// no real model is ever called. They use the installed Google Chrome, so no browser is downloaded.
export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  timeout: 90_000,
  expect: { timeout: 20_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [['list']],
  use: { channel: 'chrome', headless: true, trace: 'off', locale: 'es-MX' },
  projects: [
    { name: 'desktop', use: { viewport: { width: 1360, height: 860 } } },
    { name: 'mobile', testMatch: ['**/visual.spec.ts'], use: { ...devices['Pixel 7'], channel: 'chrome', viewport: { width: 390, height: 844 } } },
  ],
})
