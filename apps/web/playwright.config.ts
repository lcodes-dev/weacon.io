import { defineConfig, devices } from "@playwright/test"

export default defineConfig({
  testDir: "e2e",
  use: { ...devices["iPhone 13"], defaultBrowserType: "chromium", baseURL: "http://127.0.0.1:8091" },
  webServer: {
    command: "rm -rf target/e2e.* && cargo run -q -p weacon-server > target/e2e.log 2>&1",
    cwd: "../..",
    url: "http://127.0.0.1:8091/api/health",
    timeout: 300_000,
    env: { WEACON_ADDR: "127.0.0.1:8091", WEACON_URL: "http://127.0.0.1:8091", WEACON_DB: "target/e2e.db", WEACON_KEY_FILE: "target/e2e.key" },
  },
})
