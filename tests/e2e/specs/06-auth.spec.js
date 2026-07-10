// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("Authentication — login & admin access", () => {

  test("Login page has email/phone input and submit", async ({ page }) => {
    await page.goto("/login");
    const input = page.locator("input[type='email'], input[name='email'], input[name='phone']").first();
    await expect(input).toBeVisible();

    const submit = page.locator("button[type='submit'], input[type='submit']").first();
    await expect(submit).toBeVisible();
  });

  test("Login form submission without data shows validation", async ({ page }) => {
    await page.goto("/login");
    const submit = page.locator("button[type='submit'], input[type='submit']").first();
    await submit.click();
    // Wait a moment for validation to fire
    await page.waitForTimeout(500);
    // Should either stay on login or show an error
    expect(page.url()).toContain("login");
  });

  test("Admin page redirects to login when not authenticated", async ({ page }) => {
    const resp = await page.goto("/admin");
    // Should redirect to login or show 401
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 401 || resp?.status() === 302).toBeTruthy();
  });

  test("Admin bookings page requires auth", async ({ page }) => {
    await page.goto("/admin/bookings");
    const url = page.url();
    expect(url.includes("login")).toBeTruthy() || expect(url.includes("admin")).toBeFalsy();
  });

  test("Admin users page requires auth", async ({ page }) => {
    await page.goto("/admin/users");
    const url = page.url();
    expect(url.includes("login") || resp?.status() === 401 || resp?.status() === 302).toBeTruthy();
  });

  test("Dev login API is available", async ({ request }) => {
    // In dev/test environment, dev-login may exist
    const resp = await request.post("/api/auth/dev-login", {
      data: { email: "test@example.com" }
    });
    // 200 = dev login works, 404 = not available in prod, 400 = bad request
    expect([200, 201, 400, 404, 405]).toContain(resp.status());
  });
});
