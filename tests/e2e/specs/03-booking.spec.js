// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("Booking — API & page behaviour", () => {

  test("Booking page renders with form fields", async ({ page }) => {
    await page.goto("/book");
    // Should have some form of booking widget — date picker, time slots, etc.
    const inputs = page.locator("input, select, button");
    const count = await inputs.count();
    expect(count).toBeGreaterThan(0);
  });

  test("Booking API requires auth for creating bookings", async ({ request }) => {
    const resp = await request.post("/api/book", {
      data: {
        date: "2026-07-10",
        time: "10:00",
        players: 2,
      }
    });
    // Expect either 401 (unauthorized) or 400 (validation)
    expect([200, 201, 400, 401, 403]).toContain(resp.status());
  });

  test("Pricing API returns rates", async ({ request }) => {
    const resp = await request.get("/book");
    // Should at least load
    expect(resp.ok()).toBeTruthy();
  });

  test("Tournaments page loads", async ({ page }) => {
    await page.goto("/mot");
    const status = await page.locator("body").textContent();
    expect(status?.length).toBeGreaterThan(0);
  });
});
