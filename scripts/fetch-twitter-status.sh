#!/usr/bin/env bash
# Fetch one X status via warm Brave profile (pw/profile-x).
#
# Usage:
#   scripts/fetch-twitter-status.sh https://x.com/user/status/123
#   scripts/fetch-twitter-status.sh 123
# Prints JSON {ok,status_id,url,author,text,detail} to stdout.
set -euo pipefail
TARGET="${1:?x status url or numeric id required}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

unset PLAYWRIGHT_BROWSERS_PATH

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [[ -s "${NVM_DIR}/nvm.sh" ]]; then
  # shellcheck source=/dev/null
  . "${NVM_DIR}/nvm.sh"
  nvm use default >/dev/null 2>&1 || true
fi

NPX="$(command -v npx || true)"
if [[ -z "${NPX}" ]]; then
  echo "npx not found; enable nvm or put Node on PATH" >&2
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

GOLD="${ITCY_TWITTER_PROFILE_DIR:-${ROOT}/pw/profile-x}"
COOKIES="${GOLD}/Default/Cookies"
if [[ ! -d "${GOLD}" ]]; then
  echo "twitter gold profile missing: ${GOLD} (log in headed Brave once)" >&2
  exit 1
fi
if [[ ! -f "${COOKIES}" ]]; then
  echo "twitter gold Cookies missing: ${COOKIES}" >&2
  exit 1
fi
if [[ -e "${GOLD}/SingletonLock" ]]; then
  echo "twitter gold profile locked (close Brave using ${GOLD}; do not kill -9)" >&2
  exit 1
fi

if ! python3 - "${COOKIES}" <<'PY'
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
need = {"auth_token", "ct0"}
missing = sorted(need - names)
if missing:
    print(
        "twitter session cold (missing cookies: "
        + ", ".join(missing)
        + "). Log in headed once on gold pw/profile-x, then close Brave with the window X.",
        file=sys.stderr,
    )
    raise SystemExit(2)
PY
then
  exit 2
fi

STATUS_URL=""
STATUS_ID=""
if [[ "$TARGET" =~ ^https?:// ]]; then
  STATUS_URL="$TARGET"
elif [[ "$TARGET" =~ ^[0-9]+$ ]]; then
  STATUS_ID="$TARGET"
else
  echo "pass an https://x.com/…/status/… URL or a numeric status id" >&2
  exit 2
fi

RUN_ROOT="${ITCY_TWITTER_RUN_DIR:-${ROOT}/pw/profile-x-run}"
mkdir -p "${RUN_ROOT}"
WORK="${RUN_ROOT}/status-$$"
CDP_PORT="${ITCY_TWITTER_STATUS_CDP_PORT:-${ITCY_TWITTER_CDP_PORT:-9225}}"
BRAVE_PID=""

cleanup() {
  if [[ -n "${BRAVE_PID}" ]] && kill -0 "${BRAVE_PID}" 2>/dev/null; then
    kill -TERM "${BRAVE_PID}" 2>/dev/null || true
  fi
  pkill -TERM -f "user-data-dir=${WORK}" 2>/dev/null || true
  for _ in $(seq 1 20); do
    if ! pgrep -f "user-data-dir=${WORK}" >/dev/null 2>&1; then
      break
    fi
    sleep 0.25
  done
  rm -rf "${WORK}" 2>/dev/null || true
  sleep 0.5
  rm -rf "${WORK}" 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p "${WORK}"
if command -v rsync >/dev/null 2>&1; then
  rsync -a \
    --delete \
    --exclude=SingletonLock \
    --exclude=SingletonCookie \
    --exclude=SingletonSocket \
    --exclude='Singleton*' \
    "${GOLD}/" "${WORK}/"
else
  cp -a "${GOLD}/." "${WORK}/"
  rm -f "${WORK}/SingletonLock" "${WORK}/SingletonCookie" "${WORK}/SingletonSocket" \
    "${WORK}"/Singleton* 2>/dev/null || true
fi

rm -rf "${WORK}/Default/Sessions" \
  "${WORK}/Default/Session Storage" \
  "${WORK}/Default/Sessions" 2>/dev/null || true
rm -f \
  "${WORK}/Default/Current Session" \
  "${WORK}/Default/Last Session" \
  "${WORK}/Default/Current Tabs" \
  "${WORK}/Default/Last Tabs" \
  "${WORK}/Default/"*"Session" \
  "${WORK}/Default/"*"Tabs" 2>/dev/null || true

export npm_config_loglevel=error
"${NPX}" -y -p playwright true >/dev/null 2>&1 || true
PW_JSON="$(ls -dt "${HOME}/.npm/_npx/"*/node_modules/playwright/package.json 2>/dev/null | head -1 || true)"
if [[ -z "${PW_JSON}" || ! -f "${PW_JSON}" ]]; then
  echo "playwright package not found under ~/.npm/_npx (npx -y -p playwright failed)" >&2
  exit 1
fi

HEADLESS="${ITCY_TWITTER_HEADLESS:-0}"
BRAVE_ARGS=(
  --user-data-dir="${WORK}"
  --remote-debugging-port="${CDP_PORT}"
  --no-first-run
  --no-default-browser-check
  --disable-session-crashed-bubble
  --disable-blink-features=AutomationControlled
  --no-startup-window
)
if [[ "${HEADLESS}" == "1" ]]; then
  BRAVE_ARGS+=(--headless=new)
fi

nohup "${BROWSER_BIN}" "${BRAVE_ARGS[@]}" \
  >/tmp/itcy-twitter-status-brave-$$.log 2>&1 &
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
  echo "Brave CDP not ready on :${CDP_PORT} (see /tmp/itcy-twitter-status-brave-$$.log)" >&2
  exit 1
fi

cd "${ROOT}"
env PLAYWRIGHT_REQUIRE_FROM="${PW_JSON}" ITCY_TWITTER_CDP_URL="http://127.0.0.1:${CDP_PORT}" \
  ITCY_TWITTER_STATUS_URL="${STATUS_URL}" ITCY_TWITTER_STATUS_ID="${STATUS_ID}" \
  node "${ROOT}/scripts/fetch-twitter-status.mjs"
