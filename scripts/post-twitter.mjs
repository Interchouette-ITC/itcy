// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

// X/Twitter production ship via CDP attach to real Brave (work profile copy).
// Invoked by scripts/post-twitter.sh - prints JSON {ok,status_id,url,reply_url,detail}.
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  detectPostRejectReason,
  normalizeShipText,
  resolvePostedStatus,
  statusIdNewer,
  stripQuotedStatusUrl,
} from "./x-ship-resolve.mjs";

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
const inReplyToId = (process.env.ITCY_TWITTER_IN_REPLY_TO_STATUS_ID || "").trim();

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
  // CDP keeps the event loop alive; exit so the shell trap can kill Brave.
  process.exit(0);
}

async function composerLooksLike(box, text) {
  const got = normalizeShipText(
    (await box.innerText()).replace(/\u200b/g, "")
  );
  const want = normalizeShipText(text);
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

async function readToastHref(page) {
  try {
    return await page.evaluate(() => {
      const a = document.querySelector(
        '[data-testid="toast"] a[href*="/status/"]'
      );
      return a ? a.getAttribute("href") : null;
    });
  } catch {
    return null;
  }
}

/**
 * Prefer the compose-dialog Post button over the home inline composer.
 * Combined `.first()` was DOM-order roulette and often hit the wrong control.
 */
async function findPostButton(scope) {
  const primary = scope.locator('[data-testid="tweetButton"]').first();
  if (await primary.isVisible().catch(() => false)) {
    return primary;
  }
  const inline = scope.locator('[data-testid="tweetButtonInline"]').first();
  if (await inline.isVisible().catch(() => false)) {
    return inline;
  }
  return scope
    .locator('[data-testid="tweetButton"], [data-testid="tweetButtonInline"]')
    .first();
}

async function waitPostButtonEnabled(btn, waitPage) {
  for (let i = 0; i < 40; i++) {
    const disabled = await btn.getAttribute("aria-disabled");
    if (disabled !== "true") return;
    await waitPage.waitForTimeout(250);
  }
  const disabled = await btn.getAttribute("aria-disabled");
  if (disabled === "true") {
    fail(
      "Post button stayed disabled (composer rejected the text; often over 280 weighted characters)"
    );
  }
}

async function composerStillOpen(scope) {
  return scope
    .locator('[data-testid="tweetTextarea_0"]')
    .first()
    .isVisible()
    .catch(() => false);
}

async function failIfRejected(waitPage, label) {
  const reason = detectPostRejectReason(
    await waitPage.evaluate(() => document.body?.innerText || "")
  );
  if (!reason) return;
  const cap = await captureOverlay(waitPage, label);
  fail(`X rejected Post: ${reason} screenshot=${relArtifact(cap.png)}`);
}

/**
 * Submit compose and return the toast status href when X shows one.
 * Prefer Ctrl+Enter (Draft.js), then the dialog Post button - not force-click
 * on whichever tweetButton* appears first in the full page DOM.
 * Must capture toast HERE: resolveAfterPost navigates away and destroys it.
 * @returns {Promise<string|null>}
 */
async function clickPost(scope) {
  const waitPage =
    typeof scope.waitForTimeout === "function" ? scope : scope.page();
  const btn = await findPostButton(scope);
  await btn.waitFor({ state: "visible", timeout: 15000 });
  await waitPostButtonEnabled(btn, waitPage);

  const box = scope.locator('[data-testid="tweetTextarea_0"]').first();
  await box.click({ timeout: 8000 }).catch(() => {});
  // Native X shortcut - more reliable than force-clicking a covered Post button.
  await waitPage.keyboard.press("Control+Enter");

  let toastHref = null;
  for (let i = 0; i < 48; i++) {
    if (looksQuotePaywall(await waitPage.content())) {
      const cap = await captureOverlay(waitPage, "quote-paywall-after-post");
      fail(
        `X blocked Quote after Post click. screenshot=${relArtifact(cap.png)}`
      );
    }
    await failIfRejected(waitPage, "post-rejected");
    toastHref = await readToastHref(waitPage);
    const boxUp = await composerStillOpen(scope);
    if (toastHref || !boxUp) {
      if (!toastHref) {
        for (let j = 0; j < 12; j++) {
          toastHref = await readToastHref(waitPage);
          if (toastHref) break;
          await waitPage.waitForTimeout(200);
        }
      }
      return toastHref;
    }
    // XPOST-20260829-000094: do not submit again here. Control+Enter already
    // posted the root; a second Post retries the same body and never opens the
    // root status to post the overflow reply.
    await waitPage.waitForTimeout(250);
  }
  await failIfRejected(waitPage, "post-rejected");
  const cap = await captureOverlay(waitPage, "post-no-confirm");
  const still = await composerStillOpen(scope);
  fail(
    `Post did not clear composer or show toast (composer_still_open=${still}; tweet was not created). screenshot=${relArtifact(cap.png)}`
  );
  return null;
}

async function scanProfileTimeline(page) {
  return page.evaluate(() => {
    const toastEl = document.querySelector(
      '[data-testid="toast"] a[href*="/status/"]'
    );
    const articles = [...document.querySelectorAll("article")].map((art) => {
      const raw = (art.innerText || "").replace(/\s+/g, " ").trim();
      const head = raw.slice(0, 96);
      const pinned = /pinned|épinglé/i.test(head);
      const statusHrefs = [
        ...art.querySelectorAll('a[href*="/status/"]'),
      ].map((a) => a.getAttribute("href"));
      return {
        pinned,
        statusHrefs,
        snippet: raw.slice(0, 280),
      };
    });
    return {
      toast: toastEl ? toastEl.getAttribute("href") : null,
      articles,
    };
  });
}

async function latestOwnOnProfile(page, excludeIds, beforeId) {
  const scan = await scanProfileTimeline(page);
  return resolvePostedStatus({
    toastHref: null,
    scan,
    excludeIds,
    beforeId: beforeId || "",
  });
}

/**
 * Prefer toast href from clickPost. Profile poll is fallback (reload, longer wait).
 * @param {string|null} toastHref
 */
async function resolveAfterPost(page, excludeIds, beforeId, toastHref) {
  const fromToast = resolvePostedStatus({
    toastHref,
    scan: { toast: null, articles: [] },
    excludeIds,
    beforeId: beforeId || "",
  });
  if (fromToast) return fromToast;

  for (let i = 0; i < 30; i++) {
    if (i === 0 || i % 5 === 0) {
      await page.goto("https://x.com/Interchouette", {
        waitUntil: "domcontentloaded",
        timeout: 60000,
      });
      await page.waitForTimeout(900);
    }
    const found = await latestOwnOnProfile(page, excludeIds, beforeId);
    if (found) return found;
    await page.waitForTimeout(1000);
  }
  return null;
}

async function postReply(page, replyText, parent, excludeIds) {
  await page.goto(parent.url, {
    waitUntil: "domcontentloaded",
    timeout: 60000,
  });
  await page.waitForTimeout(1000);
  const btn = page.locator('[data-testid="reply"]').first();
  await btn.waitFor({ state: "visible", timeout: 20000 });
  await btn.click();
  await page.waitForTimeout(800);
  await fillComposer(page, replyText);
  const toastHref = await clickPost(page);
  const skip = excludeIds.concat([parent.id]);
  return resolveAfterPost(page, skip, parent.id, toastHref);
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
  if (inReplyToId && quoteId) {
    fail("cannot set both ITCY_TWITTER_QUOTE_STATUS_ID and ITCY_TWITTER_IN_REPLY_TO_STATUS_ID");
  }
  if (inReplyToId && replyFile) {
    fail("threaded reply ship does not take ITCY_TWITTER_REPLY_TEXT_FILE");
  }
  // Cited status already attached in composer; do not also paste that URL.
  text = stripQuotedStatusUrl(text, quoteId);

  const browser = await chromium.connectOverCDP(cdpUrl);
  const context = browser.contexts()[0] || (await browser.newContext());
  const page = context.pages()[0] || (await context.newPage());

  try {
    await goProfile(page);
    const before = await latestOwnOnProfile(page, [], "");
    const beforeId = before && before.id ? before.id : "0";

    if (inReplyToId) {
      const parent = {
        id: inReplyToId,
        url: `https://x.com/i/web/status/${inReplyToId}`,
      };
      const found = await postReply(page, text, parent, []);
      if (!found) {
        const cap = await captureOverlay(page, "in-reply-resolve-miss");
        fail(
          `reply posted but could not resolve status under ${inReplyToId}. screenshot=${relArtifact(cap.png)}`
        );
      }
      if (!statusIdNewer(found.id, beforeId)) {
        fail(`resolve picked non-newer id ${found.id} (before=${beforeId})`);
      }
      ok(found.id, found.url, `brave in-reply ok (parent=${inReplyToId})`, null);
      return;
    }

    const excludeIds = quoteId ? [quoteId] : [];

    let rootToastHref = null;
    if (quoteId) {
      const scope = await openQuoteComposer(page, quoteId);
      await fillComposer(page, text, scope);
      rootToastHref = await clickPost(scope);
    } else {
      await clickProfilePost(page);
      if (looksLoggedOut(page.url(), await page.content())) {
        fail("logged out on compose");
      }
      if (isStatusPermalink(page.url())) {
        fail(
          "compose landed on a status permalink; refusing to type into that reply box"
        );
      }
      const dialog = page
        .locator('[role="dialog"]')
        .filter({ has: page.locator('[data-testid="tweetTextarea_0"]') })
        .first();
      const scope = (await dialog.isVisible().catch(() => false))
        ? dialog
        : page;
      await fillComposer(page, text, scope);
      rootToastHref = await clickPost(scope);
    }

    const found = await resolveAfterPost(
      page,
      excludeIds,
      beforeId,
      rootToastHref
    );
    if (!found) {
      const cap = await captureOverlay(page, "resolve-miss");
      fail(
        `posted but could not resolve status (toast=${rootToastHref || "none"}; no newer own than ${beforeId}). screenshot=${relArtifact(cap.png)}`
      );
    }
    if (!statusIdNewer(found.id, beforeId)) {
      fail(`resolve picked non-newer id ${found.id} (before=${beforeId})`);
    }

    let replyFound = null;
    if (replyFile) {
      let replyText = "";
      try {
        replyText = fs.readFileSync(replyFile, "utf8").trim();
      } catch (e) {
        fail(`read reply file: ${e}`);
      }
      if (!replyText) {
        fail("overflow reply file empty (root would ship without tags/URL)");
      }
      replyFound = await postReply(page, replyText, found, excludeIds);
      if (!replyFound) {
        const cap = await captureOverlay(page, "reply-resolve-miss");
        fail(
          `root ${found.id} live but overflow reply did not resolve. screenshot=${relArtifact(cap.png)}`
        );
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
  }
  // Do not call Playwright browser teardown here: on CDP that kills Brave while
  // the shell still owns cleanup. process.exit after ok/fail drops the CDP
  // handle; post-twitter.sh trap then SIGTERM the Brave we started.
}



main();
