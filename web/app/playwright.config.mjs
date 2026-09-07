import { defineConfig } from "playwright/test"

// An external app can be supplied; otherwise start an isolated local dev server.
const baseURL = process.env.E2E_BASE_URL ?? "http://127.0.0.1:3217"

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.spec.mjs",
  workers: 1,
  use: {
    baseURL,
    browserName: "chromium",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: process.env.E2E_BASE_URL ? undefined : {
    command: "pnpm run dev --hostname 127.0.0.1 --port 3217",
    url: baseURL,
    timeout: 120_000,
    reuseExistingServer: false,
  },
})
