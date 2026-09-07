import { test, expect } from "playwright/test"

// Exercise the real app/router with deterministic API responses. No backend,
// DNS, SMTP delivery or existing test account is needed for this UI contract.
const org = { id: 1, name: "Bounce tests", permalink: "bounce-tests" }
const server = { id: 7, name: "Mail", permalink: "mail", mode: "Live" }
const base = `/orgs/${org.permalink}/servers/${server.permalink}/messaging`
const timestamp = "2026-09-01T12:00:00Z"
const original = {
  id: 101, scope: "outgoing", status: "Bounced", bounce: false,
  subject: "Original test email", mail_from: "sender@example.com",
  rcpt_to: "recipient@example.net", created_at: timestamp, message_id: "test@example.com",
}
const correlated = {
  ...original, id: 102, scope: "incoming", status: "Processed", bounce: true,
  subject: "Delivery failed", bounce_for_id: 101, bounce_category: "hard",
  bounce_correlated_at: timestamp,
}
const unmatched = {
  ...correlated, id: 103, subject: "Unmatched bounce", status: "HardFail",
  bounce_for_id: null, bounce_correlated_at: null, bounce_category: "undetermined",
}

async function mockApi(page, { missingOriginal = false } = {}) {
  const unexpected = []
  const errors = []
  page.on("pageerror", (error) => errors.push(error.message))
  await page.addInitScript(() => localStorage.setItem("camelmailer.session_token", "ui-test-session"))
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname
    const success = (data) => route.fulfill({ json: { status: "success", time: 0, data } })
    const missing = () => route.fulfill({ status: 404, json: {
      status: "error", error: { code: "NotFound", message: "Message not found" },
    } })
    if (path === "/api/v2/auth/me") return success({
      user: { id: 1, first_name: "UI", last_name: "Tester", email: "ui@example.com", admin: false },
      memberships: [{ role: "owner", organization: org }],
    })
    if (path === `/api/v2/admin/organizations/${org.permalink}`) return success({ organization: org })
    if (path === `/api/v2/admin/organizations/${org.permalink}/servers`) return success({ servers: [server] })
    if (path === `/api/v2/admin/organizations/${org.permalink}/billing`) return success({ enabled: false })
    if (path === `/api/v2/admin/organizations/${org.permalink}/servers/${server.permalink}/credentials`) {
      return success({ credentials: [{ id: 1, type: "API", hold: false, key: "ui-test-api-key" }] })
    }
    const match = path.match(/^\/api\/v2\/server\/messages\/(\d+)(?:\/(\w+))?$/)
    if (match) {
      expect(route.request().headers()["x-server-api-key"]).toBe("ui-test-api-key")
      const id = Number(match[1])
      if (!match[2]) {
        const message = [original, correlated, unmatched].find((message) => message.id === id)
        return message && !(missingOriginal && id === original.id) ? success({ message }) : missing()
      }
      if (["deliveries", "opens", "clicks"].includes(match[2])) return success({ [match[2]]: [] })
      if (match[2] === "insights") return success({ checks: [] })
      if (match[2] === "raw") return success({ raw_message: Buffer.from("Subject: Test\r\n\r\nTest body").toString("base64") })
    }
    unexpected.push(path)
    return missing()
  })
  return () => {
    expect(unexpected, "unexpected API calls").toEqual([])
    expect(errors, "browser runtime errors").toEqual([])
  }
}

test("correlated bounce links to its original within the same server", async ({ page }, testInfo) => {
  const verify = await mockApi(page)
  await page.goto(`${base}/102`)
  const notice = page.getByRole("region", { name: "Bounce notification" })
  await expect(notice).toBeVisible()
  await expect(notice).toContainText("Category:Hard")
  await expect(notice.locator("time")).toHaveAttribute("datetime", timestamp)
  await page.screenshot({ path: testInfo.outputPath("bounce-notification.png") })
  const link = notice.getByRole("link", { name: "original message #101" })
  await expect(link).toHaveAttribute("href", `${base}/101`)
  await link.focus()
  await page.keyboard.press("Enter")
  await expect(page).toHaveURL(new RegExp(`${base}/101(?:\\?|$)`))
  await expect(page.getByRole("heading", { name: original.subject })).toBeVisible()
  await expect(page.getByRole("region", { name: "Bounce notification" })).toHaveCount(0)
  verify()
})

test("unmatched bounce shows no original-message link or correlation time", async ({ page }) => {
  const verify = await mockApi(page)
  await page.goto(`${base}/103`)
  const notice = page.getByRole("region", { name: "Bounce notification" })
  await expect(notice).toContainText("No original message matched.")
  await expect(notice).toContainText("Undetermined")
  await expect(notice.getByRole("link")).toHaveCount(0)
  await expect(notice.locator("time")).toHaveCount(0)
  verify()
})

test("an unavailable original uses the existing message-load error", async ({ page }) => {
  const verify = await mockApi(page, { missingOriginal: true })
  await page.goto(`${base}/102`)
  await page.getByRole("link", { name: "original message #101" }).click()
  await expect(page.getByText("This message could not be loaded.", { exact: true })).toBeVisible({ timeout: 15_000 })
  verify()
})
