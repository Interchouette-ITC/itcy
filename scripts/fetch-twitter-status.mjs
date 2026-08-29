// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

// Fetch one X status via CDP Brave (work profile copy).
// Prints JSON {ok,status_id,url,author,text,detail} to stdout.
import { createRequire } from "node:module";

const requireFrom = process.env.PLAYWRIGHT_REQUIRE_FROM;
if (!requireFrom) {
  throw new Error("PLAYWRIGHT_REQUIRE_FROM unset (playwright package.json path)");
}
const require = createRequire(requireFrom);
const { chromium } = require("playwright");

const cdpUrl = process.env.ITCY_TWITTER_CDP_URL || "http://127.0.0.1:9224";
const statusUrl = (process.env.ITCY_TWITTER_STATUS_URL || "").trim();
const statusId = (process.env.ITCY_TWITTER_STATUS_ID || "").trim();

function fail(detail) {
  process.stdout.write(JSON.stringify({ ok: false, detail: String(detail) }) + "\n");
  process.exit(1);
}

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

function resolveUrl() {
  if (statusUrl) return statusUrl.split("?")[0].split("#")[0];
  if (statusId && /^\d+$/.test(statusId)) {
    return `https://x.com/i/web/status/${statusId}`;
  }
  return "";
}

function idFromUrl(url) {
  const m = String(url || "").match(/\/status\/(\d+)/);
  return m ? m[1] : "";
}

async function extractStatus(page, wantId) {
  return page.evaluate((id) => {
    const articles = Array.from(document.querySelectorAll('article[data-testid="tweet"]'));
    for (const article of articles) {
      const links = Array.from(article.querySelectorAll('a[href*="/status/"]'));
      let href = "";
      for (const a of links) {
        const h = a.getAttribute("href") || "";
        if (/\/status\/\d+/.test(h) && !/\/(?:analytics|photo|video)\b/.test(h)) {
          href = h.startsWith("http") ? h.split("?")[0] : `https://x.com${h.split("?")[0]}`;
          break;
        }
      }
      const sid = (href.match(/\/status\/(\d+)/) || [])[1] || "";
      if (id && sid && sid !== id) continue;
      let display = "";
      let handle = "";
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
      if (!handle) {
        const m = href.match(/x\.com\/([^/]+)\/status\/\d+/i);
        if (m && m[1] && m[1] !== "i" && m[1] !== "intent") handle = m[1];
      }
      const tw = article.querySelector('[data-testid="tweetText"]');
      let body = tw ? (tw.innerText || "").trim() : "";
      if (body.length < 4) {
        body = (article.innerText || "").trim();
      }
      body = body.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
      if (!body) continue;
      const author = handle
        ? display
          ? `${display} (@${handle})`
          : `@${handle}`
        : display || "unknown";
      return {
        status_id: sid || id || "",
        url: href || (id ? `https://x.com/i/web/status/${id}` : ""),
        author,
        text: body,
      };
    }
    return null;
  }, wantId || "");
}

async function main() {
  const url = resolveUrl();
  if (!url) fail("ITCY_TWITTER_STATUS_URL or ITCY_TWITTER_STATUS_ID required");
  const wantId = statusId || idFromUrl(url);

  const browser = await chromium.connectOverCDP(cdpUrl);
  const context = browser.contexts()[0] || (await browser.newContext());
  const page = context.pages()[0] || (await context.newPage());

  try {
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60000 });
    await page.waitForTimeout(1500);
    if (looksLoggedOut(page.url(), await page.content())) {
      fail("logged out on status fetch");
    }
    let found = null;
    for (let i = 0; i < 8; i++) {
      found = await extractStatus(page, wantId);
      if (found && found.text) break;
      await page.waitForTimeout(700);
    }
    if (!found || !found.text) {
      fail(`could not read status text for ${wantId || url}`);
    }
    process.stdout.write(
      JSON.stringify({
        ok: true,
        status_id: found.status_id || wantId,
        url: found.url || url,
        author: found.author,
        text: found.text,
        detail: "brave status fetch ok",
      }) + "\n"
    );
  } catch (e) {
    fail(e && e.message ? e.message : String(e));
  }
  // Leave Brave up; the shell trap owns the process lifetime.
}


main();
