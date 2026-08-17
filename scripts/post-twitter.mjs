// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

// X/Twitter production ship via CDP attach to real Brave (work profile copy).
// Invoked by scripts/post-twitter.sh - prints JSON {ok,status_id,url,reply_url,detail}.
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const requireFrom = process.env.PLAYWRIGHT_REQUIRE_FROM;
if (!requireFrom) {
  throw new Error("PLAYWRIGHT_REQUIRE_FROM unset (playwright package.json path)");
}
const require = createRequire(requireFrom);
const { chromium } = require("playwright");

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cdpUrl = process.env.ITCY_TWITTER_CDP_URL || "http://127.0.0.1:9224";
const textFile = process.env.ITCY_TWITTER_POST_TEXT_FILE || "";
const quoteId = (process.env.ITCY_TWITTER_QUOTE_STATUS_ID || "").trim();
const replyFile = (process.env.ITCY_TWITTER_REPLY_TEXT_FILE || "").trim();

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

function fail(detail) {
  process.stdout.write(JSON.stringify({ ok: false, detail: String(detail) }) + "\n");
  process.exit(1);
}

function ok(statusId, url, detail, reply) {
  const payload = { ok: true, status_id: statusId, url, detail: detail || "" };
  if (reply && reply.id) {
    payload.reply_status_id = reply.id;
    payload.reply_url = reply.url;
  }
  process.stdout.write(JSON.stringify(payload) + "\n");
}

async function composerLooksLike(box, text) {
  const got = (await box.innerText()).replace(/\u200b/g, "").replace(/\s+/g, " ");
  const want = text.replace(/\s+/g, " ").trim();
  const needle = want.slice(0, Math.min(24, want.length));
  return needle.length > 0 && got.includes(needle);
}

function looksQuotePaywall(html) {
  const h = (html || "").toLowerCase();
  return (
    h.includes("subscribe to quote") ||
    h.includes("upgrade to quote") ||
    h.includes("s'abonner pour citer") ||
    h.includes("s’abonner pour citer")
  );
}

function isStatusPermalink(url) {
  const u = url || "";
  return /\/status\/\d+/.test(u) && !u.includes("/compose/");
}

function shipDebugDir() {
  const fromEnv = (process.env.ITCY_X_SHIP_DEBUG_DIR || "").trim();
  return fromEnv || path.join(ROOT, "pw/screenshots/x-ship");
}

function relArtifact(abs) {
  const prefix = ROOT + path.sep;
  return abs.startsWith(prefix) ? abs.slice(prefix.length) : abs;
}

async function captureOverlay(page, label) {
  const dir = shipDebugDir();
  fs.mkdirSync(dir, { recursive: true });
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const png = path.join(dir, `${stamp}-${label}.png`);
  const htmlPath = path.join(dir, `${stamp}-${label}.html`);
  await page.screenshot({ path: png, fullPage: true });
  fs.writeFileSync(htmlPath, await page.content());
  return { png, html: htmlPath };
}

function laterButton(page) {
  // Exact-ish label only. Do not match Skip to home timeline (/skip/) or
  // tweet copy that contains "later".
  return page
    .getByRole("button", { name: /^(maybe\s+)?later$|^plus tard$|^not now$/i })
    .or(page.getByText(/^(maybe\s+)?later$|^plus tard$/i))
    .first();
}

async function clickOrDump(page, loc, label) {
  try {
    await loc.click({ timeout: 8000, force: true });
  } catch (e) {
    const cap = await captureOverlay(page, `click-${label}`);
    fail(
      `${label} click failed: ${e && e.message ? e.message : e}. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
}

async function tryDismissOverlay(page) {
  const later = laterButton(page);
  const laterUp = await later.isVisible().catch(() => false);
  if (!laterUp) {
    return false;
  }
  await captureOverlay(page, "overlay");
  await clickOrDump(page, later, "Later");
  await page.waitForTimeout(1000);
  if (await later.isVisible().catch(() => false)) {
    const cap2 = await captureOverlay(page, "overlay-still");
    fail(
      `Later still visible after click. screenshot=${relArtifact(cap2.png)} html=${relArtifact(cap2.html)}`
    );
  }
  return true;
}

async function goProfile(page) {
  await page.goto("https://x.com/Interchouette", {
    waitUntil: "domcontentloaded",
    timeout: 60000,
  });
  await page.waitForTimeout(2000);
  if (looksLoggedOut(page.url(), await page.content())) {
    fail("logged out on profile");
  }
  await tryDismissOverlay(page);
}

async function clickProfilePost(page) {
  const postBtn = page.locator('[data-testid="SideNav_NewTweet_Button"]').first();
  try {
    await postBtn.waitFor({ state: "visible", timeout: 20000 });
  } catch {
    const cap = await captureOverlay(page, "no-profile-post");
    fail(
      `profile Post button missing. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
  await clickOrDump(page, postBtn, "profile-post");
  await page.waitForTimeout(1500);
  await tryDismissOverlay(page);
}

async function fillComposer(page, text, root) {
  const scope = root || page;
  const box = scope.locator('[data-testid="tweetTextarea_0"]').first();
  await box.waitFor({ state: "visible", timeout: 30000 });
  await clickOrDump(page, box, "composer");
  // keyboard.type() sends key events and mangles supplementary-plane emoji.
  await page.keyboard.press("Control+A");
  await page.keyboard.insertText(text);
  if (await composerLooksLike(box, text)) return;
  const dialogOnly = scope !== page;
  await page.evaluate(
    ({ value, dialogOnly: inDialog }) => {
      const rootEl = inDialog
        ? document.querySelector('[role="dialog"]')
        : document;
      if (!rootEl) return;
      const el = rootEl.querySelector('[data-testid="tweetTextarea_0"]');
      if (!el) return;
      el.focus();
      document.execCommand("selectAll", false, undefined);
      document.execCommand("insertText", false, value);
    },
    { value: text, dialogOnly }
  );
  if (!(await composerLooksLike(box, text))) {
    fail("composer did not accept tweet text (emoji insert failed)");
  }
}

async function quoteComposerScope(page, qid) {
  await tryDismissOverlay(page);
  if (looksQuotePaywall(await page.content())) {
    const cap = await captureOverlay(page, "quote-paywall");
    fail(
      `X blocked Quote (subscribe to quote). screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
  if (isStatusPermalink(page.url())) {
    fail("compose landed on a status permalink; refusing to type into that reply box");
  }
  const box = page.locator('[data-testid="tweetTextarea_0"]').first();
  await box.waitFor({ state: "visible", timeout: 15000 });
  const html = await page.content();
  if (!html.includes(`/status/${qid}`)) {
    const cap = await captureOverlay(page, "quote-missing");
    fail(
      `compose opened without the cited tweet attached. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
  const dialog = page
    .locator('[role="dialog"]')
    .filter({ has: page.locator('[data-testid="tweetTextarea_0"]') })
    .first();
  if (await dialog.isVisible().catch(() => false)) {
    return dialog;
  }
  return page;
}

async function openQuoteComposer(page, qid) {
  await goProfile(page);
  await page.goto(
    `https://x.com/intent/post?url=${encodeURIComponent(`https://x.com/i/status/${qid}`)}`,
    {
      waitUntil: "domcontentloaded",
      timeout: 60000,
    }
  );
  await page.waitForTimeout(2000);
  if (looksLoggedOut(page.url(), await page.content())) {
    fail("logged out on quote compose");
  }
  await tryDismissOverlay(page);
  return quoteComposerScope(page, qid);
}

async function clickPost(scope) {
  const waitPage =
    typeof scope.waitForTimeout === "function" ? scope : scope.page();
  const btn = scope
    .locator('[data-testid="tweetButtonInline"], [data-testid="tweetButton"]')
    .first();
  await btn.waitFor({ state: "visible", timeout: 15000 });
  for (let i = 0; i < 40; i++) {
    const disabled = await btn.getAttribute("aria-disabled");
    if (disabled !== "true") break;
    await waitPage.waitForTimeout(250);
  }
  const disabled = await btn.getAttribute("aria-disabled");
  if (disabled === "true") {
    fail(
      "Post button stayed disabled (composer rejected the text; often over 280 weighted characters)"
    );
  }
  await clickOrDump(waitPage, btn, "post");
}

function statusFromHref(href) {
  if (!href) return null;
  const path = String(href).split("?")[0];
  const m = path.match(/\/(i|[A-Za-z0-9_]+)\/status\/(\d+)/);
  if (!m) return null;
  const handle = m[1];
  const id = m[2];
  const url =
    handle.toLowerCase() === "i"
      ? `https://x.com/i/status/${id}`
      : `https://x.com/${handle}/status/${id}`;
  return { id, handle: handle.toLowerCase(), url };
}

function pickPostedStatus(candidates, excludeIds) {
  const skip = (s) => excludeIds.includes(s.id);
  const own = candidates.find((s) => s.handle === "interchouette" && !skip(s));
  if (own) return own;
  return candidates.find((s) => !skip(s)) || null;
}

async function readStatusFromPage(page, excludeIds, tries) {
  for (let i = 0; i < tries; i++) {
    const hrefs = await page.evaluate(() => {
      const toast = document.querySelector(
        '[data-testid="toast"] a[href*="/status/"]'
      );
      const all = [...document.querySelectorAll('a[href*="/status/"]')].map(
        (a) => a.getAttribute("href")
      );
      return {
        page: location.href,
        toast: toast ? toast.getAttribute("href") : null,
        all,
      };
    });
    const cands = [];
    const toast = statusFromHref(hrefs.toast);
    if (toast) cands.push(toast);
    const fromUrl = statusFromHref(hrefs.page);
    if (fromUrl) cands.push(fromUrl);
    for (const h of hrefs.all || []) {
      const s = statusFromHref(h);
      if (s) cands.push(s);
    }
    const picked = pickPostedStatus(cands, excludeIds);
    if (picked) return picked;
    await page.waitForTimeout(500);
  }
  return null;
}

async function postReply(page, replyText, parent, excludeIds) {
  await page.goto(parent.url, {
    waitUntil: "domcontentloaded",
    timeout: 60000,
  });
  await page.waitForTimeout(2000);
  const btn = page.locator('[data-testid="reply"]').first();
  await btn.waitFor({ state: "visible", timeout: 20000 });
  await btn.click();
  await page.waitForTimeout(1000);
  await fillComposer(page, replyText);
  await clickPost(page);
  await page.waitForTimeout(2000);
  const skip = excludeIds.concat([parent.id]);
  let found = await readStatusFromPage(page, skip, 12);
  if (!found) {
    await page.goto("https://x.com/Interchouette", {
      waitUntil: "domcontentloaded",
      timeout: 60000,
    });
    await page.waitForTimeout(3500);
    found = await readStatusFromPage(page, skip, 16);
  }
  return found;
}

async function main() {
  if (!textFile) fail("ITCY_TWITTER_POST_TEXT_FILE unset");
  let text;
  try {
    text = fs.readFileSync(textFile, "utf8").trim();
  } catch (e) {
    fail(`read tweet file: ${e}`);
  }
  if (!text) fail("empty tweet text");

  const browser = await chromium.connectOverCDP(cdpUrl);
  const context = browser.contexts()[0] || (await browser.newContext());
  const page = context.pages()[0] || (await context.newPage());

  try {
    if (quoteId) {
      const scope = await openQuoteComposer(page, quoteId);
      await fillComposer(page, text, scope);
      await clickPost(scope);
    } else {
      await goProfile(page);
      await clickProfilePost(page);
      if (looksLoggedOut(page.url(), await page.content())) {
        fail("logged out on compose");
      }
      if (isStatusPermalink(page.url())) {
        fail("compose landed on a status permalink; refusing to type into that reply box");
      }
      const dialog = page
        .locator('[role="dialog"]')
        .filter({ has: page.locator('[data-testid="tweetTextarea_0"]') })
        .first();
      const scope = (await dialog.isVisible().catch(() => false)) ? dialog : page;
      await fillComposer(page, text, scope);
      await clickPost(scope);
    }

    await page.waitForTimeout(2000);
    const excludeIds = quoteId ? [quoteId] : [];
    // Quote compose stays on the source status; do not treat that URL as ours.
    let found = await readStatusFromPage(page, excludeIds, quoteId ? 8 : 24);
    if (!found) {
      await page.goto("https://x.com/Interchouette", {
        waitUntil: "domcontentloaded",
        timeout: 60000,
      });
      await page.waitForTimeout(3500);
      found = await readStatusFromPage(page, excludeIds, 16);
    }
    if (!found) {
      fail("posted but could not resolve status URL");
    }
    let replyFound = null;
    if (replyFile) {
      let replyText = "";
      try {
        replyText = fs.readFileSync(replyFile, "utf8").trim();
      } catch (e) {
        fail(`read reply file: ${e}`);
      }
      if (replyText) {
        replyFound = await postReply(page, replyText, found, excludeIds);
      }
    }
    ok(
      found.id,
      found.url,
      replyFound ? "brave post+reply ok" : "brave post ok",
      replyFound
    );
  } catch (e) {
    fail(e && e.message ? e.message : String(e));
  } finally {
    try {
      await browser.close();
    } catch (_) {
      /* ignore */
    }
  }
}

main();
