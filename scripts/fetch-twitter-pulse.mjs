// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

// X/Twitter discovery via CDP attach to real Brave (work profile copy only).
// Invoked by scripts/fetch-twitter-pulse.sh - prints JSON hits to stdout.
// Search always adds lang:en so results stay English-biased.
// Digest home: For you tab then Following tab (chronological follows), then searches (f=live).
import { createRequire } from "node:module";

const requireFrom = process.env.PLAYWRIGHT_REQUIRE_FROM;
if (!requireFrom) {
  throw new Error("PLAYWRIGHT_REQUIRE_FROM unset (playwright package.json path)");
}
const require = createRequire(requireFrom);
const { chromium } = require("playwright");

const mode = process.argv[2];
const queries = process.argv.slice(3).filter((q) => q && q.trim());
const cdpUrl = process.env.ITCY_TWITTER_CDP_URL || "http://127.0.0.1:9224";
const TARGET_HITS = 20;
const MAX_PER_SEARCH = 3;
const MAX_SCROLLS = 12;

function looksLoggedOut(url, html) {
  const u = (url || "").toLowerCase();
  if (
    u.includes("/i/flow/login") ||
    u.includes("/i/jf/onboarding") ||
    u.includes("/login") ||
    u.includes("mode=login")
  ) {
    return true;
  }
  const h = (html || "").toLowerCase();
  return (
    h.includes("sign in to x") ||
    h.includes("log in to x") ||
    h.includes("e-mail ou nom d’utilisateur") ||
    h.includes("e-mail ou nom d'utilisateur")
  );
}

function withEnglishLang(query) {
  const q = query.trim();
  if (/\blang:en\b/i.test(q)) return q;
  return `${q} lang:en`;
}

function handleFromStatusUrl(url) {
  const m = String(url || "").match(/x\.com\/([^/]+)\/status\/\d+/i);
  if (!m) return "";
  const h = m[1];
  if (!h || h === "i" || h === "intent") return "";
  return h;
}

async function collectOnce(page, lane, queryTag, limit) {
  return page.evaluate(
    ({ laneName, queryName, maxHits }) => {
      const out = [];
      const seen = new Set();
      for (const a of document.querySelectorAll('a[href*="/status/"]')) {
        const href = a.getAttribute("href") || "";
        if (!/\/status\/\d+/.test(href)) continue;
        if (/\/status\/\d+\/(?:analytics|photo|video)\b/.test(href)) continue;
        const url = href.startsWith("http")
          ? href.split("?")[0]
          : `https://x.com${href.split("?")[0]}`;
        if (seen.has(url)) continue;
        const article = a.closest("article");
        let display = "";
        let handle = "";
        let body = "";
        if (article) {
          const un = article.querySelector('[data-testid="User-Name"]');
          if (un) {
            const lines = (un.innerText || "")
              .split("\n")
              .map((s) => s.trim())
              .filter(Boolean);
            display = lines[0] || "";
            const at = lines.find((l) => l.startsWith("@"));
            if (at) handle = at.replace(/^@/, "").trim();
          }
          const tw = article.querySelector('[data-testid="tweetText"]');
          body = tw ? (tw.innerText || "").trim() : "";
          if (body.length < 12) {
            body = (article.innerText || "").trim();
          }
        }
        if (!handle) {
          const m = url.match(/x\.com\/([^/]+)\/status\/\d+/i);
          if (m && m[1] && m[1] !== "i" && m[1] !== "intent") handle = m[1];
        }
        if (body.length < 12) {
          body = (a.innerText || "").trim();
        }
        body = body.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
        if (body.length < 12) body = url;
        const who = handle
          ? display
            ? `${display} @${handle}`
            : `@${handle}`
          : display || "unknown";
        const title = `${who} · ${body}`;
        seen.add(url);
        out.push({
          title,
          url,
          subject: body.split(/\s+/).slice(0, 8).join(" "),
          detail: body,
          lane: laneName,
          query: queryName || "",
        });
        if (out.length >= maxHits) break;
      }
      return out;
    },
    { laneName: lane, queryName: queryTag || "", maxHits: limit },
  );
}

async function collectWithScroll(page, lane, queryTag, target) {
  const seen = new Set();
  const hits = [];
  for (let i = 0; i < MAX_SCROLLS && hits.length < target; i++) {
    const batch = await collectOnce(page, lane, queryTag, target);
    for (const h of batch) {
      if (seen.has(h.url)) continue;
      seen.add(h.url);
      if (!h.query && queryTag) h.query = queryTag;
      const fromUrl = handleFromStatusUrl(h.url);
      if (fromUrl && !/@[A-Za-z0-9_]/.test(h.title)) {
        h.title = `@${fromUrl} · ${h.detail || h.title}`;
      }
      hits.push(h);
      if (hits.length >= target) break;
    }
    if (hits.length >= target) break;
    await page.evaluate(() => window.scrollBy(0, Math.floor(window.innerHeight * 0.9)));
    await page.waitForTimeout(1200);
  }
  return hits.slice(0, target);
}

function assertLoggedIn(page, html, where) {
  if (looksLoggedOut(page.url(), html)) {
    throw new Error(
      `session cold under CDP: login wall on ${where} (gold vault untouched; re-login on pw/profile-x)`,
    );
  }
}

async function clickHomeTab(page, nameRe) {
  const tab = page.getByRole("tab", { name: nameRe }).first();
  if ((await tab.count()) === 0) {
    throw new Error(`home tab not found: ${nameRe}`);
  }
  const selected = await tab.getAttribute("aria-selected");
  if (selected !== "true") {
    await tab.click();
    await page.waitForTimeout(2000);
  }
  return tab;
}

/**
 * Following tab has aria-haspopup=menu and a caret. Second click opens
 * Popular / Recent - never use "Manage timelines".
 */
async function selectFollowingRecent(page) {
  const tab = await clickHomeTab(page, /^(Following|Abonnements)$/i);
  // Open caret menu on the already-selected Following tab.
  await tab.click();
  await page.waitForTimeout(1200);
  const expanded = await tab.getAttribute("aria-expanded");
  if (expanded !== "true") {
    // Fallback: click the SVG caret inside the tab.
    await tab.locator("svg").last().click({ force: true }).catch(() => {});
    await page.waitForTimeout(1200);
  }
  const recent = page.getByRole("menuitem", { name: /^(Recent|Latest|Récents)$/i }).first();
  if ((await recent.count()) === 0) {
    console.error(
      "twitter pulse: Following menu open but no Recent item; scraping Following as-is",
    );
    await page.keyboard.press("Escape").catch(() => {});
    return false;
  }
  await recent.click();
  await page.waitForTimeout(2000);
  return true;
}

async function scrapeHomeLanes(page) {
  await page.goto("https://x.com/home", {
    waitUntil: "domcontentloaded",
    timeout: 60000,
  });
  await page.waitForTimeout(2000);
  assertLoggedIn(page, await page.content(), "/home");

  await clickHomeTab(page, /^(For you|Pour vous)$/i);
  const forYou = await collectWithScroll(page, "for_you", "", TARGET_HITS);

  await selectFollowingRecent(page);
  const following = await collectWithScroll(page, "following", "", TARGET_HITS);

  // For you wins on URL collision.
  const seen = new Set(forYou.map((h) => h.url));
  const followingDedup = following.filter((h) => !seen.has(h.url));
  return forYou.concat(followingDedup);
}

async function scrapeSearches(page, searchQueries) {
  if (searchQueries.length === 0) return [];
  const seenUrl = new Set();
  const hits = [];
  for (const raw of searchQueries.slice(0, 20)) {
    const qRaw = raw.trim();
    const q = encodeURIComponent(withEnglishLang(qRaw));
    await page.goto(
      `https://x.com/search?q=${q}&src=typed_query&f=live&lang=en`,
      { waitUntil: "domcontentloaded", timeout: 60000 },
    );
    await page.waitForTimeout(2000);
    assertLoggedIn(page, await page.content(), "search");
    const batch = await collectWithScroll(page, "twitter", qRaw, MAX_PER_SEARCH);
    for (const h of batch) {
      if (seenUrl.has(h.url)) continue;
      seenUrl.add(h.url);
      hits.push(h);
    }
  }
  return hits;
}

async function singlePage(context) {
  const existing = context.pages().filter((p) => !p.isClosed());
  if (existing[0]) return existing[0];
  return context.newPage();
}

const browser = await chromium.connectOverCDP(cdpUrl);
const context = browser.contexts()[0] || (await browser.newContext());
await context.addInitScript(() => {
  Object.defineProperty(navigator, "webdriver", {
    get: () => undefined,
  });
});
const page = await singlePage(context);

let hits = [];
try {
  if (mode === "following") {
    hits = await scrapeHomeLanes(page);
  } else if (mode === "search") {
    if (queries.length === 0) throw new Error("search query empty");
    hits = await scrapeSearches(page, queries);
  } else if (mode === "digest") {
    hits = await scrapeHomeLanes(page);
    hits = hits.concat(await scrapeSearches(page, queries));
  } else {
    throw new Error(`unknown mode ${mode}`);
  }
  process.stdout.write(JSON.stringify(hits));
} finally {
  // Leave Brave up; the shell trap owns the process lifetime.
}
