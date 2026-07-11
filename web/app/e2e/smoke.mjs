// E2E smoke: drive the real UI against the real Docker backend.
// Login -> create org -> create server -> domain (+verify) -> API
// credential -> send a message -> see it in the messages list + stats.
import { chromium } from "playwright"

const BASE = "http://localhost:5173"
const shots = "/tmp/claude-0/-home-user-postal/12945ef2-c532-583a-bffd-b3c4178dac96/scratchpad"
let step = 0
const browser = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium" })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
page.setDefaultTimeout(15000)

async function shot(name) {
  step += 1
  await page.screenshot({ path: `${shots}/e2e-${String(step).padStart(2, "0")}-${name}.png` })
}

try {
  // ---- login
  await page.goto(`${BASE}/login`)
  await page.fill("#email", "e2e@example.com")
  await page.fill("#password", "e2e-test-password-1")
  await shot("login")
  await page.click("button[type=submit]")
  await page.waitForURL(`${BASE}/`)
  await page.getByText("Your organizations").waitFor()
  await shot("dashboard")

  // ---- create org via sidebar +
  await page.getByTitle("New organization").click()
  await page.fill("#org-name", "E2E Corp")
  await page.getByRole("button", { name: "Create", exact: true }).click()
  await page.waitForURL(`${BASE}/orgs/e2e-corp`)
  await page.getByText("Mail servers").waitFor()
  await shot("org")

  // ---- create server
  await page.getByRole("button", { name: "New server" }).click()
  await page.getByPlaceholder("Production").fill("Production")
  await page.getByRole("button", { name: "Create", exact: true }).click()
  await page.waitForURL(`${BASE}/orgs/e2e-corp/servers/production`)
  await page.getByText("General").waitFor()
  await shot("server")

  // ---- domain + verify
  await page.getByRole("tab", { name: "Domains" }).click()
  await page.getByRole("button", { name: "Add domain" }).click()
  await page.getByPlaceholder("mail.acme.com").fill("e2e.example")
  await page.getByRole("button", { name: "Add", exact: true }).click()
  await page.getByRole("cell", { name: "e2e.example" }).waitFor()
  await page.getByRole("button", { name: "Verify" }).click()
  await page.getByText("verified", { exact: true }).waitFor()
  await shot("domains")

  // ---- API credential
  await page.getByRole("tab", { name: "Credentials" }).click()
  await page.getByRole("button", { name: "New credential" }).click()
  await page.getByPlaceholder("backend").fill("frontend")
  await page.getByRole("button", { name: "Create", exact: true }).click()
  await page.getByText("Shown only once").waitFor()
  await shot("credential")
  await page.getByRole("button", { name: "Done" }).click()

  // ---- send a message
  await page.getByRole("tab", { name: "Messaging" }).click()
  await page.getByText("Send a message").waitFor()
  await page.getByPlaceholder("hello@yourdomain.com").fill("hello@e2e.example")
  await page.getByPlaceholder("a@x.com, b@y.com").fill("someone@example.com")
  // subject is the first plain input after the recipient fields
  const sendCard = page.locator("div").filter({ hasText: "Send a message" }).last()
  await page.locator("input:below(:text('Subject'))").first().fill("E2E smoke test")
  await page.locator("textarea").first().fill("Sent from the React frontend.")
  void sendCard
  await page.getByRole("button", { name: "Send", exact: true }).click()
  await page.getByText(/Queued as message #/).waitFor()
  await shot("send")

  // ---- messages list shows it
  await page.getByRole("tab", { name: "Messages" }).click()
  await page.getByRole("cell", { name: "E2E smoke test" }).waitFor()
  await shot("messages")

  // message detail with deliveries opens
  await page.getByRole("cell", { name: "E2E smoke test" }).click()
  await page.getByText("Delivery attempts").waitFor()
  await shot("message-detail")
  await page.keyboard.press("Escape")

  // ---- stats
  await page.getByRole("tab", { name: "Stats", exact: true }).click()
  await page.getByText("Outgoing").first().waitFor()
  await shot("stats")

  // ---- members + invitation
  await page.goto(`${BASE}/orgs/e2e-corp/invitations`)
  await page.getByRole("button", { name: "Invite" }).click()
  await page.getByRole("dialog").locator("input").first().fill("mate@example.com")
  await page.getByRole("button", { name: "Create invitation" }).click()
  await page.getByText("Invitation link").waitFor()
  await shot("invitation")

  // ---- audit log (instance admin)
  await page.goto(`${BASE}/admin/audit`)
  await page.getByText("login.success").first().waitFor()
  await shot("audit")

  // ---- account security page renders
  await page.goto(`${BASE}/account`)
  await page.getByText("Two-factor authentication").waitFor()
  await shot("account")

  console.log("E2E SMOKE: ALL STEPS PASSED")
} catch (error) {
  await shot("FAILURE")
  console.error("E2E SMOKE FAILED:", error.message)
  process.exitCode = 1
} finally {
  await browser.close()
}
