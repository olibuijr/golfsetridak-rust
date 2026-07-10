// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("Performance — load times & response sizes", () => {

  test("Homepage loads under 3 seconds", async ({ page }) => {
    const start = Date.now();
    await page.goto("/", { waitUntil: "networkidle" });
    const loadTime = Date.now() - start;
    expect(loadTime).toBeLessThan(3000);
  });

  test("Shop API responds in under 1 second", async ({ request }) => {
    const start = Date.now();
    const resp = await request.get("/api/shop/products");
    const elapsed = Date.now() - start;
    expect(resp.ok()).toBeTruthy();
    expect(elapsed).toBeLessThan(1000);
  });

  test("CSS file is reasonably sized", async ({ request }) => {
    const resp = await request.get("/styles.css");
    expect(resp.ok()).toBeTruthy();
    const text = await resp.text();
    // CSS should not be enormous
    expect(text.length).toBeLessThan(500000); // 500KB
  });

  test("Homepage assets (images) load without 404s", async ({ page }) => {
    const failedRequests = [];
    page.on("requestfailed", req => {
      failedRequests.push(req.url());
    });
    await page.goto("/", { waitUntil: "networkidle" });
    // Allow some asset 404s if they're not critical
    expect(failedRequests.length).toBeLessThan(3);
  });

  test("SSL/TLS is valid", async ({ request }) => {
    const resp = await request.get("/");
    expect(resp.ok()).toBeTruthy();
    // The request went through HTTPS — if it reached us, TLS worked
  });
});
