// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

// Public-page HTML fetch for `/ingest` only. Ephemeral Brave; never profile-x / profile-brave.
import { createRequire } from 'node:module';

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

const browserBin = process.env.ITCY_BROWSER_EXECUTABLE || '';
const cdpUrl = process.env.ITCY_OBSCURA_CDP_URL || '';
const headed = process.env.ITCY_PUBLIC_FETCH_HEADED === '1';
const waitMs = Number(process.env.ITCY_PUBLIC_FETCH_CF_WAIT_MS || '45000');

const launchOpts = {
  executablePath: browserBin,
  chromiumSandbox: true,
  headless: !headed,
};

async function settlePastChallenge(page) {
  const deadline = Date.now() + waitMs;
  while (Date.now() < deadline) {
    const html = await page.content();
    if (
      !html.includes('challenges.cloudflare.com') &&
      !/just a moment/i.test(html) &&
      !/un instant/i.test(html)
    ) {
      return;
    }
    await page.waitForTimeout(2000);
  }
}

async function fetchOnePage(page) {
  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 60000 });
  await page
    .waitForLoadState('networkidle', { timeout: 20000 })
    .catch(() => undefined);
  await settlePastChallenge(page);
  return page.content();
}

async function fetchViaCdp() {
  const browser = await chromium.connectOverCDP(cdpUrl);
  try {
    const context = browser.contexts()[0] ?? (await browser.newContext());
    const page = await context.newPage();
    try {
      return await fetchOnePage(page);
    } finally {
      await page.close();
    }
  } finally {
    await browser.close();
  }
}

async function fetchViaBrave() {
  const browser = await chromium.launch(launchOpts);
  try {
    const context = await browser.newContext({
      viewport: { width: 1280, height: 720 },
    });
    const page = await context.newPage();
    try {
      return await fetchOnePage(page);
    } finally {
      await page.close();
      await context.close();
    }
  } finally {
    await browser.close();
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
  process.stdout.write(await fetchViaBrave());
}

await main();
