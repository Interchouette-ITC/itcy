#!/usr/bin/env bash
# Open headed Brave on the Twitter gold profile for a one-shot login.
# Does not scrape. Close with the window X when the home timeline looks OK.
# Never kill -9 the process (that can leave a cold cookie jar).
#
# Usage: scripts/open-twitter-login.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GOLD="${ITCY_TWITTER_PROFILE_DIR:-${ROOT}/pw/profile-x}"
COOKIES="${GOLD}/Default/Cookies"

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

mkdir -p "${GOLD}/Default"

if [[ -e "${GOLD}/SingletonLock" ]]; then
  echo "twitter gold profile already locked: ${GOLD}" >&2
  echo "Close that Brave window with the X (do not kill -9), then re-run if needed." >&2
  exit 1
fi

if [[ -f "${COOKIES}" ]] && python3 - "${COOKIES}" <<'PY'
import sqlite3, shutil, sys, tempfile
from pathlib import Path
src = Path(sys.argv[1])
td = tempfile.mkdtemp()
try:
    shutil.copy2(src, Path(td) / "Cookies")
    for side in ("Cookies-journal", "Cookies-wal"):
        p = src.parent / side
        if p.is_file():
            shutil.copy2(p, Path(td) / side)
    con = sqlite3.connect(f"file:{td}/Cookies?mode=ro", uri=True)
    names = {
        r[0]
        for r in con.execute(
            "select name from cookies where host_key like '%x.com%' or host_key like '%twitter%'"
        )
    }
finally:
    shutil.rmtree(td, ignore_errors=True)
raise SystemExit(0 if {"auth_token", "ct0"} <= names else 1)
PY
then
  echo "twitter vault already warm: ${GOLD}"
  echo "No browser opened. Scrapes use a disposable copy (scripts/fetch-twitter-pulse.sh)."
  exit 0
fi

echo "Opening Brave for X login on gold profile:"
echo "  ${GOLD}"
echo "When the home timeline looks OK: close with the window X (not kill)."
echo "Then check: curl -s http://127.0.0.1:4700/status | python3 -c 'import json,sys; print(json.load(sys.stdin).get(\"twitter\"))'"

nohup "${BROWSER_BIN}" \
  --user-data-dir="${GOLD}" \
  --no-first-run \
  --no-default-browser-check \
  "https://x.com/home" \
  >/tmp/itcy-twitter-login.log 2>&1 &
echo "brave_pid=$! (log: /tmp/itcy-twitter-login.log)"
