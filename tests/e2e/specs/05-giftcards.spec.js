// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("Gift cards — page & API", () => {

  test("Gift card page loads with redemption form", async ({ page }) => {
    await page.goto("/gjafabref");
    const body = await page.locator("body").textContent() || "";
    // Should mention gift cards in Icelandic
    const hasGiftCardContent = body.includes("gjafabréf") || body.includes("gjafakort");
    expect(hasGiftCardContent).toBeTruthy();
  });

  test("Gift card lookup API rejects invalid codes gracefully", async ({ request }) => {
    const resp = await request.get("/api/cart/gift-card/lookup?code=INVALID-CODE-12345");
    // Should return 404 or 400 for invalid codes, not 500
    expect([400, 404, 200, 401]).toContain(resp.status());
    if (resp.status() === 200) {
      const body = await resp.json();
      // If by some miracle it returns, should indicate invalid
      expect(body).not.toHaveProperty("valid", true);
    }
  });

  test("Gift card redeem API handles invalid codes", async ({ request }) => {
    const resp = await request.post("/api/cart/gift-card/redeem", {
      data: { code: "INVALID-CODE-12345" }
    });
    expect([400, 404, 401, 403]).toContain(resp.status());
  });
});
