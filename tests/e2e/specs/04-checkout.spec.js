// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("Checkout — flows & redirects", () => {

  test("Checkout page redirects to login if not authenticated", async ({ page }) => {
    await page.goto("/checkout");
    // Should either show login or redirect to login
    const url = page.url();
    const body = await page.locator("body").textContent() || "";
    const isOnLogin = url.includes("login") || body.includes("innskrá") || body.includes("netfang");
    expect(isOnLogin).toBeTruthy();
  });

  test("Bank transfer checkout page loads", async ({ page }) => {
    const resp = await page.goto("/checkout/bank-transfer");
    // May redirect if not authenticated — that's fine
    expect(resp?.status() === 200 || resp?.status() === 302 || resp?.status() === 301).toBeTruthy();
  });

  test("Landsbankinn checkout page loads", async ({ page }) => {
    const resp = await page.goto("/checkout/landsbankinn");
    expect(resp?.status() === 200 || resp?.status() === 302 || resp?.status() === 301).toBeTruthy();
  });

  test("Checkout success page loads", async ({ page }) => {
    const resp = await page.goto("/checkout/success");
    expect(resp?.status() === 200 || resp?.status() === 302).toBeTruthy();
  });

  test("Checkout cancel page loads", async ({ page }) => {
    const resp = await page.goto("/checkout/cancel");
    expect(resp?.status() === 200 || resp?.status() === 302).toBeTruthy();
  });
});
