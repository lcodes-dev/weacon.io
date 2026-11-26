import { expect, test, type Page } from "@playwright/test"
import { readFileSync } from "node:fs"

// The owner signs up with the code the server prints to its log, while mail isn't set up.
test.beforeEach(async ({ page }) => {
  const email = `ada${Date.now()}@example.com`
  await page.request.post("/api/sign-in/code", { data: { email, name: "Ada Dorn" } })
  const code = readFileSync("../../target/e2e.log", "utf8").match(/\b\d{6}\b/g)!.at(-1)
  await page.request.post("/api/sign-in/verify", { data: { email, code } })
})

/** Opens a page and says what doesn't fit a phone: what scrolls sideways, and what to tap is under 44px tall. */
async function misfits(page: Page, path: string) {
  await page.goto(path)
  await page.waitForLoadState("networkidle")
  const problems = await page.evaluate(() => {
    const found: string[] = []
    for (const element of document.querySelectorAll<HTMLElement>("body *")) {
      const style = getComputedStyle(element)
      const box = element.getBoundingClientRect()
      if (box.width === 0 || style.visibility === "hidden" || element.closest("[aria-hidden=true], [inert]")) continue
      if (/auto|scroll/.test(style.overflowX) && element.scrollWidth > element.clientWidth + 1 && !element.closest("pre, code, [data-scrolls]"))
        found.push(`scrolls sideways: ${element.outerHTML.slice(0, 80)}`)
      // A link written as underlined text is exempt, as WCAG 2.5.8 allows for links in text.
      const tapped = element.matches("a, button, select, input:not([type=checkbox], [type=radio]), textarea, summary")
      if (tapped && style.textDecorationLine !== "underline" && box.height < 44) found.push(`${Math.round(box.height)}px tall: ${element.outerHTML.slice(0, 80)}`)
    }
    if (document.documentElement.scrollWidth > innerWidth) found.push("the page scrolls sideways")
    return found
  })
  return problems.map((problem) => `${path}: ${problem}`)
}

test("every page fits a phone", async ({ page }) => {
  const { teams } = (await (await page.request.get("/api/session")).json()).user
  const team = `/api/teams/${teams[0].id}`
  const project = await (await page.request.post(`${team}/projects`, { data: { name: "Storefront", key: "SF", branches: [] } })).json()
  const task = await (await page.request.post(`${team}/projects/${project.id}/tasks`, { data: { title: "Returns link on mobile" } })).json()
  const group = await (await page.request.post(`${team}/groups`, { data: { members: [], name: "Gift wrap", project_id: project.id } })).json()

  const problems = []
  for (const path of [
    "/",
    `/projects/${project.id}`,
    `/projects/${project.id}/tasks`,
    `/projects/${project.id}/tasks?view=board`,
    `/projects/${project.id}/tasks?task=${task.id}`,
    `/projects/${project.id}/docs`,
    `/groups/${group.id}`,
    "/groups/new",
    "/chats/new",
    "/settings/team",
    "/settings/machines",
    "/settings/mail",
  ])
    problems.push(...(await misfits(page, path)))
  expect(problems).toEqual([])
})

test("the sidebar is a drawer on a phone", async ({ page }) => {
  await page.goto("/settings/team")
  const sidebar = page.getByRole("complementary", { name: "Sidebar" })
  await expect(sidebar).not.toBeInViewport()

  await page.getByRole("button", { name: "Menu" }).click()
  await expect(sidebar).toBeInViewport()
  await page.keyboard.press("Escape")
  await expect(sidebar).not.toBeInViewport()

  await page.getByRole("button", { name: "Menu" }).click()
  await sidebar.getByRole("link", { name: "Your machines" }).or(sidebar.getByRole("link", { name: "Add a machine" })).click()
  await expect(page).toHaveURL(/settings\/machines/)
  await expect(sidebar).not.toBeInViewport()
})
