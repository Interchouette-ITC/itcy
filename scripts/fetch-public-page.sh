#!/usr/bin/env bash
# Public-page HTML fetch for `/ingest` only (no login). Does not use profile-x or profile-brave.
# Usage: scripts/fetch-public-page.sh <url>
# Optional: ITCY_PUBLIC_FETCH_HEADED=1 for headed Brave (Cloudflare checkbox on OUP, etc.).
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

export ITCY_ROOT="${ROOT}"
export PLAYWRIGHT_REQUIRE_FROM="${PW_JSON}"

if [[ "${BROWSER_NAME}" == "obscura" ]]; then
  # shellcheck source=scripts/lib/obscura-serve.sh
  source "${ROOT}/scripts/lib/obscura-serve.sh"
  obscura_ensure_serve
  export ITCY_OBSCURA_CDP_URL="$(obscura_cdp_base_url)"
  unset ITCY_BROWSER_EXECUTABLE
else
  unset ITCY_OBSCURA_CDP_URL
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
  export ITCY_BROWSER_EXECUTABLE="${BROWSER_BIN}"
fi

cd "${ROOT}"
exec "${NODE}" "${ROOT}/scripts/lib/fetch-public-page.mjs" "${URL}"
