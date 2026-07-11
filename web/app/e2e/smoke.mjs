// E2E smoke: drive the Next.js UI against the real Docker backend.
// Marketing landing -> login -> create org -> server -> domain (+verify)
// -> API credential -> send -> messages list/detail -> stats -> invitation
// -> audit log -> account page.
import { chromium } from "playwright"

const BASE = process.env.E2E_BASE_URL ?? "http://localhost:3000"
const shots = process.env.E2E_SHOTS ?? "/tmp"
let step = 0
const browser = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium" })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
page.setDefaultTimeout(20000)

async function shot(name) {
  step += 1
  await page.screenshot({ path: `${shots}/e2e-${String(step).padStart(2, "0")}-${name}.png` })
}

try {
  // ---- marketing landing is served by the same app
  await page.goto(`${BASE}/`)
  await page.getByText("Transactional email.").first().waitFor()
  await shot("landing")

  // ---- login
  await page.goto(`${BASE}/login`)
  await page.fill("#email", "e2e@example.com")
  await page.fill("#password", "e2e-test-password-1")
  await shot("login")
  await page.click("button[type=submit]")
  await page.waitForURL(`${BASE}/dashboard`)
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

  // ---- domain: records dialog, DNS-gated verify, operator force
  await page.getByRole("tab", { name: "Domains" }).click()
  await page.waitForURL(`${BASE}/orgs/e2e-corp/servers/production/domains`)
  await page.getByRole("button", { name: "Add domain" }).click()
  await page.getByPlaceholder("mail.acme.com").fill("e2e.example")
  await page.getByRole("button", { name: "Add", exact: true }).click()
  // creating a domain opens the DNS-records dialog (verification/SPF/DKIM)
  await page.getByText("DNS records for e2e.example").waitFor()
  await page.getByText("camelmailer-verification=").first().waitFor()
  await shot("domain-records")
  await page.keyboard.press("Escape")
  await page.getByRole("cell", { name: "e2e.example" }).waitFor()
  // without the published TXT record, Verify surfaces the API's message
  await page.getByRole("button", { name: "Verify" }).click()
  await page
    .getByText(/Domain ownership is not proven yet|Could not check the TXT record/)
    .first()
    .waitFor()
  await shot("domain-verify-rejected")
  // operator escape hatch: mint a machine key and force-verify with it
  const forced = await page.evaluate(async () => {
    const token = localStorage.getItem("camelmailer.session_token")
    const created = await fetch("/api/v2/admin/admin_api_keys", {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
      body: JSON.stringify({ name: "e2e-force" }),
    }).then((r) => r.json())
    const verify = await fetch(
      "/api/v2/admin/organizations/e2e-corp/servers/production/domains/e2e.example/verify",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "X-Admin-API-Key": created.data.admin_api_key.key,
        },
        body: JSON.stringify({ force: true }),
      },
    ).then((r) => r.json())
    return verify.data?.domain?.verified === true
  })
  if (!forced) throw new Error("force-verify with the machine key failed")
  await page.reload()
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
  await page.locator("input:below(:text('Subject'))").first().fill("E2E smoke test")
  await page.locator("textarea").first().fill("Sent from the Next.js frontend.")
  await page.getByRole("button", { name: "Send", exact: true }).click()
  await page.getByText(/Queued as message #/).waitFor()
  await shot("send")

  // ---- messages list + detail
  await page.getByRole("tab", { name: "Messages" }).click()
  await page.getByRole("cell", { name: "E2E smoke test" }).waitFor()
  await page.getByRole("cell", { name: "E2E smoke test" }).click()
  await page.getByText("Delivery attempts").waitFor()
  await shot("message-detail")
  await page.keyboard.press("Escape")

  // ---- stats
  await page.getByRole("tab", { name: "Stats", exact: true }).click()
  await page.getByText("Outgoing").first().waitFor()
  await shot("stats")

  // ---- invitation
  await page.goto(`${BASE}/orgs/e2e-corp/invitations`)
  await page.getByRole("button", { name: "Invite" }).click()
  await page.getByRole("dialog").locator("input").first().fill("mate@example.com")
  await page.getByRole("button", { name: "Create invitation" }).click()
  await page.getByText("Invitation link").waitFor()
  await shot("invitation")

  // ---- audit log + account
  await page.goto(`${BASE}/admin/audit`)
  await page.getByText("login.success").first().waitFor()
  await shot("audit")
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
