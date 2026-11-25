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
  composerTextMatches,
  readShipPayloads,
  normalizeShipText,
  postedTextMatches,
  replyHasParent,
  resolvePostedStatus,
  statusIdNewer,
  stripQuotedStatusUrl,
} from "./x-ship-resolve.mjs";
import {
  activeComposerScope,
  composerBox,
  composerText,
  profilePostButton,
  scanProfileTimeline,
  submitPostButton,
} from "./x-ship-ui.mjs";

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
let pendingThreadRoot = null;

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
  if (pendingThreadRoot && !String(detail).includes("partial X thread:")) {
    detail = `partial X thread: root=${pendingThreadRoot.id} url=${pendingThreadRoot.url}; ${detail}; do not repost root`;
  }
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
  return composerTextMatches(await composerText(box), text);
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
  const postBtn = profilePostButton(page);
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
  const box = composerBox(scope);
  try {
    await box.waitFor({ state: "visible", timeout: 30000 });
  } catch (e) {
    const cap = await captureOverlay(page, "composer-missing");
    fail(
      `composer missing: ${e.message}. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
  await clickOrDump(page, box, "composer");
  if (await box.evaluate((el) => el.tagName === "TEXTAREA")) {
    await box.fill(text);
    if (!(await composerLooksLike(box, text))) {
      fail("composer did not accept tweet text (textarea fill failed)");
    }
    return;
  }
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
  const box = composerBox(page);
  await box.waitFor({ state: "visible", timeout: 15000 });
  const html = await page.content();
  if (!html.includes(`/status/${qid}`)) {
    const cap = await captureOverlay(page, "quote-missing");
    fail(
      `compose opened without the cited tweet attached. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
  return activeComposerScope(page);
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
  return submitPostButton(scope);
}

async function waitPostButtonEnabled(btn, waitPage) {
  for (let i = 0; i < 40; i++) {
    if (await btn.isEnabled()) return;
    await waitPage.waitForTimeout(250);
  }
  if (!(await btn.isEnabled())) {
    await failIfRejected(waitPage, "post-disabled-rejected");
    const cap = await captureOverlay(waitPage, "post-disabled");
    fail(
      `Post/Reply button stayed disabled in active composer. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
    );
  }
}

async function composerStillOpen(scope) {
  return composerBox(scope)
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

  const box = composerBox(scope);
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

async function latestOwnOnProfile(page, excludeIds, beforeId, expectedText) {
  const scan = await scanProfileTimeline(page);
  scan.toast = null;
  return resolvePostedStatus({
    toastHref: null,
    scan,
    excludeIds,
    beforeId: beforeId || "",
    expectedText,
  });
}

/**
 * Prefer toast href from clickPost. Profile poll is fallback (reload, longer wait).
 * @param {string|null} toastHref
 */
async function resolveAfterPost(page, excludeIds, beforeId, toastHref, expectedText) {
  const fromToast = resolvePostedStatus({
    toastHref,
    scan: { toast: null, articles: [] },
    excludeIds,
    beforeId: beforeId || "",
  });
  if (fromToast) {
    await page.goto(fromToast.url, { waitUntil: "domcontentloaded", timeout: 60000 });
    await page.waitForTimeout(1000);
    const confirmed = await latestOwnOnProfile(page, excludeIds, beforeId, expectedText);
    if (confirmed && confirmed.id === fromToast.id) return confirmed;
  }

  for (let i = 0; i < 30; i++) {
    if (i === 0 || i % 5 === 0) {
      await page.goto("https://x.com/Interchouette", {
        waitUntil: "domcontentloaded",
        timeout: 60000,
      });
      await page.waitForTimeout(900);
    }
    const found = await latestOwnOnProfile(page, excludeIds, beforeId, expectedText);
    if (found) return found;
    await page.waitForTimeout(1000);
  }
  return null;
}

async function postReply(replyPage, replyText, parent, excludeIds) {
  await replyPage.goto(parent.url, {
    waitUntil: "domcontentloaded",
    timeout: 60000,
  });
  await replyPage.waitForTimeout(1000);
  if (looksLoggedOut(replyPage.url(), await replyPage.content())) {
    fail("logged out on reply status page");
  }
  if (parent.text) {
    const scan = await scanProfileTimeline(replyPage);
    const matched = scan.articles.some((art) =>
      art.statusHrefs.some((href) => href.endsWith(`/status/${parent.id}`)) &&
      postedTextMatches(art.text, parent.text)
    );
    if (!matched) fail(`reply parent ${parent.id} does not match the published root text`);
  }
  const btn = replyPage.locator(
    `a[href="/compose/post?in_reply_to=${parent.id}"]:visible`
  ).or(
    replyPage.locator("article")
      .filter({ has: replyPage.locator(`a[href$="/status/${parent.id}"]`) })
      .locator('[data-testid="reply"]:visible')
  ).first();
  await btn.waitFor({ state: "visible", timeout: 20000 });
  await btn.click();
  await replyPage.waitForTimeout(800);
  const scope = await activeComposerScope(replyPage);
  await fillComposer(replyPage, replyText, scope);
  const toastHref = await clickPost(scope);
  const skip = excludeIds.concat([parent.id]);
  const found = await resolveAfterPost(replyPage, skip, parent.id, toastHref, replyText);
  if (!found) return null;
  await replyPage.goto(found.url, { waitUntil: "domcontentloaded", timeout: 60000 });
  for (let i = 0; i < 40; i++) {
    const scan = await scanProfileTimeline(replyPage);
    if (replyHasParent(scan, found.id, parent.id)) return found;
    await replyPage.waitForTimeout(250);
  }
  const cap = await captureOverlay(replyPage, "reply-parent-mismatch");
  fail(
    `reply ${found.id} could not be confirmed under parent ${parent.id}. screenshot=${relArtifact(cap.png)} html=${relArtifact(cap.html)}`
  );
}

/** One root tab; close stray CDP tabs from profile restore. */
async function prepareRootTab(context) {
  const pages = context.pages();
  const rootPage = pages[0] || (await context.newPage());
  for (const page of pages.slice(1)) {
    await page.close().catch(() => {});
  }
  return rootPage;
}

async function closeShipTabs(rootPage, replyPage) {
  if (replyPage) {
    await replyPage.close().catch(() => {});
  }
  if (rootPage) {
    await rootPage.close().catch(() => {});
  }
}

async function postRootTweet(rootPage, text, quoteId, excludeIds, beforeId) {
  let rootToastHref = null;
  if (quoteId) {
    const scope = await openQuoteComposer(rootPage, quoteId);
    await fillComposer(rootPage, text, scope);
    rootToastHref = await clickPost(scope);
  } else {
    await clickProfilePost(rootPage);
    if (looksLoggedOut(rootPage.url(), await rootPage.content())) {
      fail("logged out on compose");
    }
    if (isStatusPermalink(rootPage.url())) {
      fail(
        "compose landed on a status permalink; refusing to type into that reply box"
      );
    }
    const scope = await activeComposerScope(rootPage);
    await fillComposer(rootPage, text, scope);
    rootToastHref = await clickPost(scope);
  }

  pendingThreadRoot = { id: "unresolved", url: rootToastHref || "unresolved" };
  const found = await resolveAfterPost(
    rootPage,
    excludeIds,
    beforeId,
    rootToastHref,
    text
  );
  if (!found) {
    const cap = await captureOverlay(rootPage, "resolve-miss");
    fail(
      `posted but could not resolve status (toast=${rootToastHref || "none"}; no newer own than ${beforeId}). screenshot=${relArtifact(cap.png)}`
    );
  }
  if (!statusIdNewer(found.id, beforeId)) {
    fail(`resolve picked non-newer id ${found.id} (before=${beforeId})`);
  }
  return { ...found, text };
}

async function main() {
  if (!textFile) fail("ITCY_TWITTER_POST_TEXT_FILE unset");
  let text;
  let replyText;
  try {
    ({ text, replyText } = readShipPayloads(
      (file) => fs.readFileSync(file, "utf8"), textFile, replyFile
    ));
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
  let rootPage = await prepareRootTab(context);
  let replyPage = null;

  try {
    await goProfile(rootPage);
    await rootPage.locator("article").first().waitFor({ state: "visible", timeout: 20000 });
    const before = await latestOwnOnProfile(rootPage, [], "");
    if (!before) fail("cannot establish newest own status before publishing");
    const beforeId = before && before.id ? before.id : "0";

    if (inReplyToId) {
      replyPage = await context.newPage();
      const parent = {
        id: inReplyToId,
        url: `https://x.com/i/web/status/${inReplyToId}`,
      };
      const found = await postReply(replyPage, text, parent, []);
      if (!found) {
        const cap = await captureOverlay(replyPage, "in-reply-resolve-miss");
        fail(
          `reply posted but could not resolve status under ${inReplyToId}. screenshot=${relArtifact(cap.png)}`
        );
      }
      if (!statusIdNewer(found.id, beforeId)) {
        fail(`resolve picked non-newer id ${found.id} (before=${beforeId})`);
      }
      await closeShipTabs(rootPage, replyPage);
      replyPage = null;
      ok(found.id, found.url, `brave in-reply ok (parent=${inReplyToId})`, null);
      return;
    }

    const excludeIds = quoteId ? [quoteId] : [];

    const found = await postRootTweet(
      rootPage,
      text,
      quoteId,
      excludeIds,
      beforeId
    );

    let replyFound = null;
    if (replyFile) {
      pendingThreadRoot = found;
      replyPage = await context.newPage();
      try {
        replyFound = await postReply(replyPage, replyText, found, excludeIds);
      } catch (e) {
        fail(`partial X thread: root=${found.id} url=${found.url}; reply failed: ${e}; do not repost root`);
      }
      if (!replyFound) {
        const cap = await captureOverlay(replyPage, "reply-resolve-miss");
        fail(
          `partial X thread: root=${found.id} url=${found.url}; overflow reply did not resolve; do not repost root. screenshot=${relArtifact(cap.png)}`
        );
      }
    }
    await closeShipTabs(rootPage, replyPage);
    rootPage = null;
    replyPage = null;
    pendingThreadRoot = null;
    ok(
      found.id,
      found.url,
      replyFound ? "brave post+reply ok" : "brave post ok",
      replyFound
    );
  } catch (e) {
    await closeShipTabs(rootPage, replyPage).catch(() => {});
    fail(e && e.message ? e.message : String(e));
  }
  // Do not call Playwright browser teardown here: on CDP that kills Brave while
  // the shell still owns cleanup. process.exit after ok/fail drops the CDP
  // handle; post-twitter.sh trap then SIGTERM the Brave we started.
}



main();
