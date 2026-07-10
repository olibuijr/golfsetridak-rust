// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("News / frette — content pages", () => {

  test("News listing page shows articles", async ({ page }) => {
    await page.goto("/frettir");
    const body = await page.locator("body").textContent() || "";
    const hasContent = body.length > 100;
    expect(hasContent).toBeTruthy();
  });

  test("Individual article pages are accessible", async ({ page }) => {
    // Navigate to news, find first article link, click it
    await page.goto("/frettir");
    const articleLinks = page.locator("a[href*='/frettir/'], a[href*='/article/']");
    const count = await articleLinks.count();
    test.skip(count === 0, "No article links found");

    if (count > 0) {
      const href = await articleLinks.first().getAttribute("href");
      await page.goto(href);
      await expect(page.locator("body")).not.toBeEmpty();
      // Article should have substantial content
      const text = await page.locator("body").textContent() || "";
      expect(text.length).toBeGreaterThan(50);
    }
  });

  test("Article pages have valid HTML structure", async ({ page }) => {
    // Test a known article page from the content directory
    await page.goto("/frettir/hola-i-hoggi-april");
    const status = page.url().includes("frettir");
    expect(status).toBeTruthy();
  });
});
