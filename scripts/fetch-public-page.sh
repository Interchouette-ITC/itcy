#!/usr/bin/env bash
# Public-page HTML fetch for ingest thin→PW (no login).
# Usage: scripts/fetch-public-page.sh <url>
# Prints HTML to stdout.
#
# Default: headless Brave/Chromium via Playwright launch (unchanged).
# Opt-in: ITCY_PW_BROWSER=obscura → obscura serve + Playwright connectOverCDP.
set -euo pipefail
URL="${1:?url required}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

unset PLAYWRIGHT_BROWSERS_PATH

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [[ -s "${NVM_DIR}/nvm.sh" ]]; then
  # shellcheck source=/dev/null
  . "${NVM_DIR}/nvm.sh"
  nvm use default >/dev/null 2>&1 || true
fi

NODE="$(command -v node || true)"
if [[ -z "${NODE}" ]]; then
  echo "node not found; enable nvm or put Node on PATH" >&2
  exit 1
fi

PW_JSON="$(ls -dt "${HOME}/.npm/_npx/"*/node_modules/playwright/package.json 2>/dev/null | head -1 || true)"
if [[ -z "${PW_JSON}" || ! -f "${PW_JSON}" ]]; then
  echo "playwright package not found under ~/.npm/_npx (expected from prior pulse/ship runs)" >&2
  exit 1
fi

BROWSER_NAME="$(echo "${ITCY_PW_BROWSER:-}" | tr '[:upper:]' '[:lower:]')"

if [[ "${BROWSER_NAME}" == "obscura" ]]; then
  ITCY_ROOT="${ROOT}"
  # shellcheck source=scripts/lib/obscura-serve.sh
  source "${ROOT}/scripts/lib/obscura-serve.sh"
  obscura_ensure_serve
  export PLAYWRIGHT_REQUIRE_FROM="${PW_JSON}"
  export ITCY_OBSCURA_CDP_URL="$(obscura_cdp_base_url)"
  cd "${ROOT}"
  exec "${NODE}" --input-type=module -e "
import { createRequire } from 'node:module';
const requireFrom = process.env.PLAYWRIGHT_REQUIRE_FROM;
if (!requireFrom) {
  throw new Error('PLAYWRIGHT_REQUIRE_FROM unset');
}
const require = createRequire(requireFrom);
const { chromium } = require('playwright');
const url = process.argv[1];
const cdpUrl = process.env.ITCY_OBSCURA_CDP_URL || 'http://127.0.0.1:9222';
const browser = await chromium.connectOverCDP(cdpUrl);
const context = browser.contexts()[0] ?? await browser.newContext();
const page = context.pages()[0] ?? await context.newPage();
await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 45000 });
process.stdout.write(await page.content());
await browser.close();
" "$URL"
fi

BROWSER_BIN="${ITCY_BROWSER_EXECUTABLE:-}"
if [[ -z "$BROWSER_BIN" ]]; then
  for candidate in /usr/bin/brave-browser /usr/bin/brave-browser-stable /usr/bin/chromium /usr/bin/chromium-browser; do
    if [[ -x "$candidate" ]]; then
      BROWSER_BIN="$candidate"
      break
    fi
  done
fi
if [[ -z "$BROWSER_BIN" || ! -x "$BROWSER_BIN" ]]; then
  echo "no system browser (brave/chromium); set ITCY_BROWSER_EXECUTABLE=" >&2
  exit 1
fi

cd "${ROOT}"
export PLAYWRIGHT_REQUIRE_FROM="${PW_JSON}"
exec "${NODE}" --input-type=module -e "
import { createRequire } from 'node:module';
const requireFrom = process.env.PLAYWRIGHT_REQUIRE_FROM;
if (!requireFrom) {
  throw new Error('PLAYWRIGHT_REQUIRE_FROM unset');
}
const require = createRequire(requireFrom);
const { chromium } = require('playwright');
const url = process.argv[1];
const exe = process.argv[2];
const browser = await chromium.launch({
  headless: true,
  executablePath: exe,
  chromiumSandbox: true,
});
const page = await browser.newPage();
await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 45000 });
process.stdout.write(await page.content());
await browser.close();
" "$URL" "$BROWSER_BIN"
