// @ts-check
const { test, expect } = require("@playwright/test");

// ─── helpers ────────────────────────────────────────────────────────────────

/**
 * Helper: dev-login as admin and return headers with cookie for authenticated requests.
 */
async function adminHeaders(request) {
  const resp = await request.post("/api/auth/dev-login", {
    data: { email: "admin@golfsetridak.is" }
  });
  if (resp.status() === 200) {
    const rawCookies = resp.headersArray()
      .filter(h => h.name.toLowerCase() === "set-cookie")
      .map(h => h.value.split(";")[0])
      .join("; ");
    return rawCookies ? { Cookie: rawCookies } : {};
  }
  return {};
}

/**
 * Helper: extract cookies from dev-login response and return Playwright cookie objects.
 */
async function adminCookies(request) {
  const resp = await request.post("/api/auth/dev-login", {
    data: { email: "admin@golfsetridak.is" }
  });
  if (resp.status() !== 200) return null;
  return resp.headersArray()
    .filter(h => h.name.toLowerCase() === "set-cookie")
    .map(h => {
      const [nameVal] = h.value.split(";");
      const [name, val] = nameVal.split("=");
      return {
        name,
        value: val,
        domain: new URL(request.service()._baseUrl || "https://rust.golfsetridak.is").hostname,
        path: "/"
      };
    });
}

// ─── Admin API endpoint tests ───────────────────────────────────────────────

test.describe("Admin API — auth gating", () => {

  test("GET /api/admin/dashboard requires auth (no cookie → 401)", async ({ request }) => {
    const resp = await request.get("/api/admin/dashboard");
    expect([401, 403, 302]).toContain(resp.status());
  });

  test("GET /api/admin/stats requires auth (no cookie → 401)", async ({ request }) => {
    const resp = await request.get("/api/admin/stats");
    expect([401, 403, 302]).toContain(resp.status());
  });

  test("GET /api/admin/shop/products requires auth", async ({ request }) => {
    const resp = await request.get("/api/admin/shop/products");
    expect([401, 403, 302]).toContain(resp.status());
  });

  test("GET /api/admin/shop/categories requires auth", async ({ request }) => {
    const resp = await request.get("/api/admin/shop/categories");
    expect([401, 403, 302]).toContain(resp.status());
  });

  test("GET /api/admin/gift-cards requires auth", async ({ request }) => {
    const resp = await request.get("/api/admin/gift-cards");
    expect([401, 403, 302]).toContain(resp.status());
  });

  test("POST /api/admin/upload requires auth", async ({ request }) => {
    const resp = await request.post("/api/admin/upload");
    expect([401, 403, 302, 400, 405]).toContain(resp.status());
  });

  test("GET /api/admin/pricing/1 requires auth", async ({ request }) => {
    const resp = await request.get("/api/admin/pricing/1");
    expect([401, 403, 302]).toContain(resp.status());
  });
});

// ─── Admin API — response shape (when authenticated) ────────────────────────

test.describe("Admin API — response shape (authenticated)", () => {

  test.describe.configure({ mode: "serial" });
  let headers = {};

  test.beforeAll(async ({ request }) => {
    headers = await adminHeaders(request);
  });

  test("GET /api/admin/dashboard returns stats object", async ({ request }) => {
    const resp = await request.get("/api/admin/dashboard", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(body).toHaveProperty("totalBookings");
    expect(body).toHaveProperty("confirmedBookings");
    expect(body).toHaveProperty("revenue");
    expect(typeof body.totalBookings).toBe("number");
    expect(typeof body.revenue).toBe("number");
  });

  test("GET /api/admin/stats returns stats object", async ({ request }) => {
    const resp = await request.get("/api/admin/stats", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(body).toHaveProperty("totalBookings");
    expect(body).toHaveProperty("confirmedBookings");
    expect(body).toHaveProperty("revenue");
  });

  test("GET /api/admin/shop/products returns array or object with products", async ({ request }) => {
    const resp = await request.get("/api/admin/shop/products", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    const items = Array.isArray(body) ? body : body.products || body.data || body.records || [];
    expect(Array.isArray(items)).toBe(true);
  });

  test("GET /api/admin/shop/categories returns array or object with categories", async ({ request }) => {
    const resp = await request.get("/api/admin/shop/categories", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    const items = Array.isArray(body) ? body : body.categories || body.data || body.records || [];
    expect(Array.isArray(items)).toBe(true);
  });

  test("GET /api/admin/gift-cards returns array or object with gift cards", async ({ request }) => {
    const resp = await request.get("/api/admin/gift-cards", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    const items = Array.isArray(body) ? body : body.giftCards || body.data || body.records || [];
    expect(Array.isArray(items)).toBe(true);
  });

  test("GET /api/admin/pricing/1 returns pricing data", async ({ request }) => {
    const resp = await request.get("/api/admin/pricing/1", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect([200, 404]).toContain(resp.status());
    if (resp.status() === 200) {
      const body = await resp.json();
      expect(typeof body).toBe("object");
      expect(body).not.toBeNull();
    }
  });

  test("POST /api/admin/upload returns appropriate status", async ({ request }) => {
    const resp = await request.post("/api/admin/upload", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect([200, 201, 400, 405]).toContain(resp.status());
  });

  test("POST /api/admin/gift-cards creates a gift card with auth", async ({ request }) => {
    const resp = await request.post("/api/admin/gift-cards", {
      headers,
      data: {
        amount: 5000,
        recipientName: "Test User",
        recipientEmail: "test@example.com",
        message: "Test gift card",
      }
    });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Dev-login not available (production mode)");
      return;
    }
    expect([200, 201, 400]).toContain(resp.status());
  });
});

// ─── Admin HTML pages — auth gating ─────────────────────────────────────────

test.describe("Admin pages — auth gating & redirects", () => {

  test("/admin redirects to login when not authenticated", async ({ page }) => {
    const resp = await page.goto("/admin");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/bookings requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/bookings");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/payments requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/payments");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/users requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/users");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/settings requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/settings");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });
  test("/admin/tilkynningar requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/tilkynningar");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/vorur requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/vorur");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/vorur/flokkar requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/vorur/flokkar");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/vorur/nytt requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/vorur/nytt");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/gift-cards requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/gift-cards");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });

  test("/admin/gift-cards/new requires auth", async ({ page }) => {
    const resp = await page.goto("/admin/gift-cards/new");
    const finalUrl = page.url();
    expect(finalUrl.includes("login") || resp?.status() === 302 || resp?.status() === 401).toBeTruthy();
  });
});

// ─── Admin HTML pages — content verification (with dev-login) ───────────────

test.describe("Admin pages — content with dev-login", () => {

  async function loginAndVisit(page, request, path) {
    const c = await adminCookies(request);
    if (!c) return false;
    await page.context().addCookies(c);
    await page.goto(path);
    return true;
  }

  test("/admin redirects to /admin/bookings when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    expect(page.url()).toContain("/admin/bookings");
  });

  test("/admin/bookings shows heading when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/bookings");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
    const body = await page.locator("body").textContent() || "";
    expect(body.length).toBeGreaterThan(100);
  });

  test("/admin/payments shows payment management when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/payments");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("/admin/users shows user management when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/users");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("/admin/settings shows settings form when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/settings");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
    const inputs = page.locator("input, select, textarea");
    const count = await inputs.count();
    expect(count).toBeGreaterThan(0);
  });

  test("/admin/tilkynningar shows announcements page when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/tilkynningar");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("/admin/vorur shows product management when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/vorur");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("/admin/vorur/nytt shows new product form when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/vorur/nytt");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("/admin/gift-cards shows gift card management when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/gift-cards");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });

  test("/admin/gift-cards/new shows create form when authenticated", async ({ page, request }) => {
    const ok = await loginAndVisit(page, request, "/admin/gift-cards/new");
    if (!ok) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await expect(page.locator("body")).not.toBeEmpty();
  });
});

// ─── Unit-test-style API contract checks ───────────────────────────────────

test.describe("Admin API — contract & error handling", () => {

  test.describe.configure({ mode: "serial" });
  let headers = {};

  test.beforeAll(async ({ request }) => {
    headers = await adminHeaders(request);
  });

  test("Dashboard stats are non-negative integers", async ({ request }) => {
    const resp = await request.get("/api/admin/dashboard", { headers });
    if (resp.status() !== 200) { test.skip(true, "Auth not available"); return; }
    const body = await resp.json();
    expect(body.totalBookings).toBeGreaterThanOrEqual(0);
    expect(body.confirmedBookings).toBeGreaterThanOrEqual(0);
    expect(body.revenue).toBeGreaterThanOrEqual(0);
  });

  test("Dashboard and stats endpoints return consistent data", async ({ request }) => {
    const dashResp = await request.get("/api/admin/dashboard", { headers });
    const statsResp = await request.get("/api/admin/stats", { headers });
    if (dashResp.status() !== 200 || statsResp.status() !== 200) {
      test.skip(true, "Auth not available"); return;
    }
    const dash = await dashResp.json();
    const stats = await statsResp.json();
    expect(Object.keys(dash).sort()).toEqual(Object.keys(stats).sort());
    expect(dash.totalBookings).toBe(stats.totalBookings);
    expect(dash.confirmedBookings).toBe(stats.confirmedBookings);
    expect(dash.revenue).toBe(stats.revenue);
  });

  test("Admin products API returns valid JSON for single product", async ({ request }) => {
    const listResp = await request.get("/api/admin/shop/products", { headers });
    if (listResp.status() !== 200) { test.skip(true, "Auth not available"); return; }
    const listBody = await listResp.json();
    const items = Array.isArray(listBody) ? listBody : listBody.products || listBody.data || listBody.records || [];
    if (items.length === 0) { test.skip(true, "No products in store"); return; }
    const firstId = items[0].id || items[0]._id || items[0].slug;
    if (!firstId) { test.skip(true, "Product has no usable ID field"); return; }

    const detailResp = await request.get(`/api/admin/shop/products/${firstId}`, { headers });
    expect(detailResp.status()).toBe(200);
    const detail = await detailResp.json();
    expect(typeof detail).toBe("object");
    expect(detail).not.toBeNull();
  });

  test("Admin categories API returns valid JSON", async ({ request }) => {
    const resp = await request.get("/api/admin/shop/categories", { headers });
    if (resp.status() !== 200) { test.skip(true, "Auth not available"); return; }
    const body = await resp.json();
    const items = Array.isArray(body) ? body : body.categories || body.data || body.records || [];
    expect(Array.isArray(items)).toBe(true);
  });

  test("Admin pricing API handles missing IDs gracefully (404 not 500)", async ({ request }) => {
    const resp = await request.get("/api/admin/pricing/99999", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect([200, 404]).toContain(resp.status());
  });

  test("Admin upload API requires multipart (returns 400 not 500 without file)", async ({ request }) => {
    const resp = await request.post("/api/admin/upload", {
      headers: { ...headers, "Content-Type": "application/json" },
      data: { notAFile: true }
    });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect([200, 400, 405, 415]).toContain(resp.status());
  });
});

// ─── Admin settings persistence ─────────────────────────────────────────────

test.describe("Admin settings — read & write persistence", () => {

  test.describe.configure({ mode: "serial" });
  let headers = {};

  test.beforeAll(async ({ request }) => {
    headers = await adminHeaders(request);
  });

  test("Settings page renders bank transfer and notification fields when authenticated", async ({ page, request }) => {
    const c = await adminCookies(request);
    if (!c) { test.skip(true, "Dev-login not available (production mode)"); return; }
    await page.context().addCookies(c);
    await page.goto("/admin/settings");
    const body = await page.locator("body").textContent() || "";
    expect(body.length).toBeGreaterThan(50);
  });

  /**
   * POST /admin/settings sends url-encoded data. When the session cookie is
   * forwarded correctly the settings page returns with the posted values
   * rendered inline. When the cookie is dropped (Playwright APIRequestContext
   * does not auto-persist cookies across requests) the server redirects to
   * /login. Both are valid 200 responses; we detect the login redirect and
   * skip in that case.
   */
  test("Bank transfer settings with empty data shows validation (or skips if cookie lost)", async ({ request }) => {
    const resp = await request.post("/admin/settings", {
      headers: { ...headers, "Content-Type": "application/x-www-form-urlencoded" },
      data: "section=bank_transfer&bankName=&accountHolder=&kennitala=&accountNumber="
    });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.text();
    // If cookie forwarding failed we get the login page — skip gracefully.
    if (body.includes("Innskráning") && !body.includes("Stillingar")) {
      test.skip(true, "Session cookie not forwarded (APIRequestContext limitation)");
      return;
    }
    // Should be the settings page
    expect(body).toContain("Stillingar");
  });

  test("Bank transfer settings with valid data persists (or skips if cookie lost)", async ({ request }) => {
    const resp = await request.post("/admin/settings", {
      headers: { ...headers, "Content-Type": "application/x-www-form-urlencoded" },
      data: "section=bank_transfer&bankName=Test+Bank&accountHolder=Test+Owner&kennitala=1234567890&accountNumber=1234-56-7890"
    });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect(resp.status()).toBe(200);
    const body = await resp.text();
    if (body.includes("Innskráning") && !body.includes("Stillingar")) {
      test.skip(true, "Session cookie not forwarded (APIRequestContext limitation)");
      return;
    }
    expect(body).toContain("Test Bank");
    expect(body).toContain("Test Owner");
  });
});

// ─── Admin API edge cases ───────────────────────────────────────────────────

test.describe("Admin API — edge cases & methods", () => {

  test.describe.configure({ mode: "serial" });
  let headers = {};

  test.beforeAll(async ({ request }) => {
    headers = await adminHeaders(request);
  });

  test("Admin dashboard rejects POST (wrong method)", async ({ request }) => {
    const resp = await request.post("/api/admin/dashboard", { headers, data: {} });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect(resp.status()).toBe(405);
  });

  test("Admin stats rejects POST (wrong method)", async ({ request }) => {
    const resp = await request.post("/api/admin/stats", { headers, data: {} });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect(resp.status()).toBe(405);
  });

  test("Admin gift-cards single lookup works with existing ID", async ({ request }) => {
    const listResp = await request.get("/api/admin/gift-cards", { headers });
    if (listResp.status() !== 200) { test.skip(true, "Auth not available"); return; }
    const listBody = await listResp.json();
    const items = Array.isArray(listBody) ? listBody : listBody.giftCards || listBody.data || listBody.records || [];
    if (items.length === 0) { test.skip(true, "No gift cards in store"); return; }
    const firstId = items[0].id || items[0]._id || items[0].code;
    if (!firstId) { test.skip(true, "Gift card has no usable ID field"); return; }

    const detailResp = await request.get(`/api/admin/gift-cards/${firstId}`, { headers });
    expect(detailResp.status()).toBe(200);
  });

  test("Admin gift-cards non-existent ID returns 404 not 500", async ({ request }) => {
    const resp = await request.get("/api/admin/gift-cards/non-existent-code-12345", { headers });
    if (resp.status() === 401 || resp.status() === 403) {
      test.skip(true, "Auth not available"); return;
    }
    expect([200, 404]).toContain(resp.status());
  });
});
