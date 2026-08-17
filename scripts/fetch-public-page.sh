#!/usr/bin/env bash
# Public-page HTML fetch for ingest thin→PW (no login; not draft Playwright MCP).
# Usage: scripts/fetch-public-page.sh <url>
# Prints HTML to stdout. Uses system Brave/Chromium + already-cached playwright
# under ~/.npm/_npx (same resolve path as fetch-twitter-pulse.sh).
# Do not npm-install or download browsers from this script.
set -euo pipefail
URL="${1:?url required}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Prefer system / nvm Playwright browsers, not an inherited browsers path.
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

# Reuse the same cached playwright package the X pulse scripts already use.
# Do not `npx -y` / install here - Node 26 eval cannot resolve from cwd anyway.
PW_JSON="$(ls -dt "${HOME}/.npm/_npx/"*/node_modules/playwright/package.json 2>/dev/null | head -1 || true)"
if [[ -z "${PW_JSON}" || ! -f "${PW_JSON}" ]]; then
  echo "playwright package not found under ~/.npm/_npx (expected from prior pulse/ship runs)" >&2
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
