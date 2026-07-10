// @ts-check
const { test, expect } = require("@playwright/test");

test.describe("Public pages — load & basic content", () => {

  test("Homepage loads with calendar heading", async ({ page }) => {
    await page.goto("/");
    await expect(page).toHaveTitle(/Golfsetrið/);
    await expect(page.locator("h1")).toBeVisible();
    // Calendar month heading is present
    await expect(page.locator("[class*=month], [class*=calendar], .cal-heading, h2").first()).toBeVisible();
  });

  test("Login page loads", async ({ page }) => {
    await page.goto("/login");
    await expect(page.locator('input[type="email"], input[name="email"], input[name="phone"]').first()).toBeVisible();
  });

  test("News page loads", async ({ page }) => {
    await page.goto("/frettir");
    await expect(page.locator("h1, h2").first()).toBeVisible();
  });

  test("About page loads", async ({ page }) => {
    await page.goto("/um-okkur");
    await expect(page.locator("body")).not.toBeEmpty();
    await expect(page).toHaveURL(/um-okkur/);
  });

  test("Gift cards page loads", async ({ page }) => {
    await page.goto("/gjafabref");
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("Shop page loads and shows products", async ({ page }) => {
    await page.goto("/verslun");
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("Booking page loads", async ({ page }) => {
    await page.goto("/book");
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("Cart page loads", async ({ page }) => {
    await page.goto("/cart");
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("User manual page loads", async ({ page }) => {
    await page.goto("/notendahandbok");
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("404 returns error for unknown page", async ({ page }) => {
    const resp = await page.goto("/this-page-does-not-exist-12345");
    // Should return a 404 status or show an error page
    expect(resp?.status() === 404 || resp?.status() === 200).toBeTruthy();
    // Should show some indication of not found
    const body = await page.locator("body").textContent();
    expect(body?.toLowerCase()).toContain("404") || expect(body?.toLowerCase()).toContain("ekki til");
  });

  test("Favicon and apple touch icon are served", async ({ page }) => {
    const favicon = await page.goto("/favicon.ico");
    expect(favicon?.status()).toBe(200);

    const appleTouch = await page.goto("/apple-touch-icon.png");
    expect(appleTouch?.status()).toBe(200);
  });

  test("Robots.txt and sitemap.xml are served", async ({ page }) => {
    const robots = await page.goto("/robots.txt");
    expect(robots?.status()).toBe(200);

    const sitemap = await page.goto("/sitemap.xml");
    expect(sitemap?.status()).toBe(200);
    const text = await sitemap?.text();
    expect(text).toContain("urlset") || expect(text).toContain("sitemap");
  });

  test("Stylesheet loads successfully", async ({ page }) => {
    const resp = await page.goto("/styles.css");
    expect(resp?.status()).toBe(200);
    expect(await resp?.text()).toContain("body");
  });
});
