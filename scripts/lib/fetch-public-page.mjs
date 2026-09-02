// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

// Public-page HTML fetch (Lane C ingest). No login.
import { createRequire } from 'node:module';
import path from 'node:path';

const requireFrom = process.env.PLAYWRIGHT_REQUIRE_FROM;
if (!requireFrom) {
  throw new Error('PLAYWRIGHT_REQUIRE_FROM unset');
}
const require = createRequire(requireFrom);
const { chromium } = require('playwright');

const url = process.argv[2];
if (!url) {
  throw new Error('usage: fetch-public-page.mjs <url>');
}

const root = process.env.ITCY_ROOT || process.cwd();
const browserBin = process.env.ITCY_BROWSER_EXECUTABLE || '';
const cdpUrl = process.env.ITCY_OBSCURA_CDP_URL || '';
const profile =
  process.env.ITCY_PW_USER_DATA_DIR ||
  path.join(root, 'pw', 'profile-public-fetch');
const forceHeaded = process.env.ITCY_PUBLIC_FETCH_HEADED === '1';
const autoHeadedOnCf = process.env.ITCY_PUBLIC_FETCH_HEADED_AUTO !== '0';
const waitMs = Number(
  process.env.ITCY_PUBLIC_FETCH_CF_WAIT_MS || (forceHeaded ? '120000' : '45000'),
);

function looksLikeCloudflare(html) {
  return (
    html.includes('challenges.cloudflare.com') ||
    html.includes('cdn-cgi/challenge-platform') ||
    /just a moment/i.test(html) ||
    /un instant/i.test(html) ||
    /performing security verification/i.test(html)
  );
}

async function settlePastChallenge(page) {
  const deadline = Date.now() + waitMs;
  while (Date.now() < deadline) {
    const html = await page.content();
    if (!looksLikeCloudflare(html)) {
      return;
    }
    await page.waitForTimeout(2000);
  }
}

async function writePageHtml(page) {
  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 60000 });
  await page
    .waitForLoadState('networkidle', { timeout: 20000 })
    .catch(() => undefined);
  await settlePastChallenge(page);
  return page.content();
}

async function fetchViaCdp() {
  const browser = await chromium.connectOverCDP(cdpUrl);
  const context = browser.contexts()[0] ?? (await browser.newContext());
  const page = context.pages()[0] ?? (await context.newPage());
  const html = await writePageHtml(page);
  await browser.close();
  return html;
}

async function fetchViaBrave(headless) {
  const context = await chromium.launchPersistentContext(profile, {
    headless,
    executablePath: browserBin,
    chromiumSandbox: true,
    viewport: { width: 1280, height: 720 },
  });
  try {
    const page = context.pages()[0] ?? (await context.newPage());
    return await writePageHtml(page);
  } finally {
    await context.close();
  }
}

async function main() {
  if (cdpUrl) {
    process.stdout.write(await fetchViaCdp());
    return;
  }
  if (!browserBin) {
    throw new Error('ITCY_BROWSER_EXECUTABLE unset and no Obscura CDP URL');
  }

  const startHeadless = !forceHeaded;
  let html = await fetchViaBrave(startHeadless);
  if (
    looksLikeCloudflare(html) &&
    autoHeadedOnCf &&
    startHeadless &&
    process.env.DISPLAY
  ) {
    // Headless hit a CF/Turnstile wall: retry headed on the same profile so the
    // operator can pass the checkbox once; cookies persist for later headless runs.
    html = await fetchViaBrave(false);
  }
  process.stdout.write(html);
}

await main();
