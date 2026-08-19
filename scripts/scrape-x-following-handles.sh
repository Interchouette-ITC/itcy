#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GOLD_PROFILE="${ITCY_TWITTER_PROFILE_DIR:-${ROOT}/pw/profile-x}"
COOKIES_DB="${GOLD_PROFILE}/Default/Cookies"
RUN_ROOT="${ITCY_TWITTER_RUN_DIR:-${ROOT}/pw/profile-x-run}"
WORK_PROFILE="${RUN_ROOT}/handles-$$"
OUT_TSV="${ITCY_HANDLES_OUT_TSV:-${ROOT}/pw/mcp/x-following-handles.tsv}"
CDP_PORT="${ITCY_TWITTER_CDP_PORT:-9224}"

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [[ -s "${NVM_DIR}/nvm.sh" ]]; then
  # shellcheck source=/dev/null
  . "${NVM_DIR}/nvm.sh"
  nvm use default >/dev/null 2>&1 || true
fi

NPX="$(command -v npx || true)"
if [[ -z "${NPX}" ]]; then
  echo "npx not found; enable node first" >&2
  exit 1
fi

BROWSER_BIN="${ITCY_BROWSER_EXECUTABLE:-}"
if [[ -z "${BROWSER_BIN}" ]]; then
  for candidate in /usr/bin/brave-browser /usr/bin/brave-browser-stable; do
    if [[ -x "$candidate" ]]; then
      BROWSER_BIN="$candidate"
      break
    fi
  done
fi
if [[ -z "${BROWSER_BIN}" || ! -x "${BROWSER_BIN}" ]]; then
  echo "no Brave binary found; set ITCY_BROWSER_EXECUTABLE" >&2
  exit 1
fi

if [[ ! -f "${COOKIES_DB}" ]]; then
  echo "missing ${COOKIES_DB}; run scripts/open-twitter-login.sh first" >&2
  exit 2
fi
if [[ -e "${GOLD_PROFILE}/SingletonLock" ]]; then
  echo "profile locked: ${GOLD_PROFILE}; close Brave window then retry" >&2
  exit 2
fi

if ! python3 - "${COOKIES_DB}" <<'PY'
import sqlite3, shutil, sys, tempfile
from pathlib import Path
src = Path(sys.argv[1])
tmp = tempfile.mkdtemp()
try:
    shutil.copy2(src, Path(tmp) / "Cookies")
    for side in ("Cookies-journal", "Cookies-wal"):
        p = src.parent / side
        if p.is_file():
            shutil.copy2(p, Path(tmp) / side)
    con = sqlite3.connect(f"file:{tmp}/Cookies?mode=ro", uri=True)
    names = {
        r[0]
        for r in con.execute(
            "select name from cookies where host_key like '%x.com%' or host_key like '%twitter%'"
        )
    }
finally:
    shutil.rmtree(tmp, ignore_errors=True)
raise SystemExit(0 if {"auth_token", "ct0"} <= names else 1)
PY
then
  echo "twitter auth cookies missing; run scripts/open-twitter-login.sh first" >&2
  exit 2
fi

cleanup() {
  if [[ -n "${BRAVE_PID:-}" ]] && kill -0 "${BRAVE_PID}" 2>/dev/null; then
    kill -TERM "${BRAVE_PID}" 2>/dev/null || true
  fi
  pkill -TERM -f "user-data-dir=${WORK_PROFILE}" 2>/dev/null || true
  rm -rf "${WORK_PROFILE}" 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p "${RUN_ROOT}" "$(dirname "${OUT_TSV}")"
if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete \
    --exclude=SingletonLock \
    --exclude=SingletonCookie \
    --exclude=SingletonSocket \
    "${GOLD_PROFILE}/" "${WORK_PROFILE}/"
else
  mkdir -p "${WORK_PROFILE}"
  cp -a "${GOLD_PROFILE}/." "${WORK_PROFILE}/"
  rm -f "${WORK_PROFILE}/SingletonLock" "${WORK_PROFILE}/SingletonCookie" "${WORK_PROFILE}/SingletonSocket"
fi

# Do not restore prior tabs from copied profile.
rm -rf "${WORK_PROFILE}/Default/Sessions" "${WORK_PROFILE}/Default/Session Storage" 2>/dev/null || true
rm -f \
  "${WORK_PROFILE}/Default/Current Session" \
  "${WORK_PROFILE}/Default/Last Session" \
  "${WORK_PROFILE}/Default/Current Tabs" \
  "${WORK_PROFILE}/Default/Last Tabs" \
  "${WORK_PROFILE}/Default/"*"Session" \
  "${WORK_PROFILE}/Default/"*"Tabs" 2>/dev/null || true

export npm_config_loglevel=error
"${NPX}" -y -p playwright true >/dev/null 2>&1 || true
PW_JSON="$(ls -dt "${HOME}/.npm/_npx/"*/node_modules/playwright/package.json 2>/dev/null | head -1 || true)"
if [[ -z "${PW_JSON}" || ! -f "${PW_JSON}" ]]; then
  echo "playwright package not found under ~/.npm/_npx" >&2
  exit 1
fi

nohup "${BROWSER_BIN}" \
  --user-data-dir="${WORK_PROFILE}" \
  --remote-debugging-port="${CDP_PORT}" \
  --no-first-run \
  --no-default-browser-check \
  --disable-session-crashed-bubble \
  --disable-blink-features=AutomationControlled \
  --no-startup-window \
  >/tmp/itcy-x-handles-brave.log 2>&1 &
BRAVE_PID=$!

ready=0
for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:${CDP_PORT}/json/version" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.2
done
if [[ "${ready}" != "1" ]]; then
  echo "Brave CDP not ready on :${CDP_PORT}" >&2
  exit 1
fi

export PLAYWRIGHT_REQUIRE_FROM="${PW_JSON}"
export ITCY_TWITTER_CDP_URL="http://127.0.0.1:${CDP_PORT}"
export ITCY_HANDLES_OUT_TSV="${OUT_TSV}"
node --input-type=module <<'JSEOF'
import fs from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(process.env.PLAYWRIGHT_REQUIRE_FROM);
const { chromium } = require("playwright");

const browser = await chromium.connectOverCDP(process.env.ITCY_TWITTER_CDP_URL);
const context = browser.contexts()[0] || await browser.newContext();
const page = context.pages()[0] || await context.newPage();

await page.goto("https://x.com/Interchouette/following", {
  waitUntil: "domcontentloaded",
  timeout: 60000
});
await page.waitForTimeout(2500);

const out = new Map();
let stale = 0;
for (let i = 0; i < 400; i += 1) {
  const rows = await page.evaluate(() =>
    Array.from(document.querySelectorAll('[data-testid="UserCell"]'))
      .map((cell) => {
        // Keep only rows that are already followed.
        const followingBtn = Array.from(cell.querySelectorAll("button"))
          .find((b) => (b.textContent || "").trim().toLowerCase() === "following");
        if (!followingBtn) return null;

        const userName = cell.querySelector('[data-testid="User-Name"]');
        const lines = (userName?.innerText || "")
          .split("\n")
          .map((x) => x.trim())
          .filter(Boolean);
        const display = lines[0] || "";
        const at = lines.find((x) => x.startsWith("@")) || "";
        const handle = at.replace(/^@/, "").trim();
        if (!handle || handle.startsWith("i/")) return null;
        return { handle, display };
      })
      .filter(Boolean)
  );

  const before = out.size;
  for (const row of rows) out.set(row.handle, row.display || row.handle);
  if (out.size === before) stale += 1;
  else stale = 0;

  if (stale >= 12) break;
  await page.evaluate(() => window.scrollBy(0, Math.floor(window.innerHeight * 1.7)));
  await page.waitForTimeout(850);
}

if (out.size === 0) {
  await page.screenshot({ path: "/tmp/itcy-x-handles-empty.png", fullPage: false });
}

const lines = Array.from(out.entries()).map(([h, d]) => `${h}\t${d}`);
fs.writeFileSync(process.env.ITCY_HANDLES_OUT_TSV, lines.join("\n") + (lines.length ? "\n" : ""), "utf8");
process.stderr.write(`handles=${out.size}\n`);
await browser.close();
JSEOF

wc -l "${OUT_TSV}"
echo "output: ${OUT_TSV}"
