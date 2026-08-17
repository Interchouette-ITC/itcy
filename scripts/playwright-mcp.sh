#!/usr/bin/env bash
# Launch Playwright MCP for ITCy browse (product child). Cursor IDE uses global Playwright MCP.
# Host browser: ITCY_PW_BROWSER=brave|chromium (default brave).
# Profile dirs stay under the product tree (not a live browser profile).
set -euo pipefail

export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [[ ! -s "${NVM_DIR}/nvm.sh" ]]; then
  echo "missing nvm at ${NVM_DIR}; install from https://github.com/nvm-sh/nvm" >&2
  exit 1
fi
# shellcheck source=/dev/null
. "${NVM_DIR}/nvm.sh"
nvm use default >/dev/null

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NPX="$(command -v npx)"
if [[ -z "${NPX}" ]]; then
  echo "npx not found after nvm use" >&2
  exit 1
fi

BROWSER_NAME="$(echo "${ITCY_PW_BROWSER:-brave}" | tr '[:upper:]' '[:lower:]')"
case "${BROWSER_NAME}" in
  brave|chromium) ;;
  *)
    echo "ITCY_PW_BROWSER must be brave|chromium (got: ${BROWSER_NAME})" >&2
    exit 1
    ;;
esac

CONFIG="${ROOT}/scripts/playwright-mcp.config.${BROWSER_NAME}.json"
if [[ ! -f "${CONFIG}" ]]; then
  CONFIG="${ROOT}/scripts/playwright-mcp.config.json"
fi
if [[ ! -f "${CONFIG}" ]]; then
  echo "missing Playwright MCP config: ${CONFIG}" >&2
  exit 1
fi

if locale -a 2>/dev/null | grep -qiE '^en_US\.utf-?8$'; then
  export LANG=en_US.UTF-8 LANGUAGE=en_US:en LC_ALL=en_US.UTF-8
else
  export LANG=C.UTF-8 LANGUAGE=C.UTF-8 LC_ALL=C.UTF-8
fi

if [[ "${BROWSER_NAME}" == "brave" ]]; then
  DEFAULT_BIN="/usr/bin/brave-browser"
  DEFAULT_PROFILE="${ROOT}/pw/profile-brave"
else
  DEFAULT_BIN="/usr/bin/chromium"
  DEFAULT_PROFILE="${ROOT}/pw/profile-chromium"
fi
if [[ ! -d "${DEFAULT_PROFILE}" && -d "${ROOT}/pw/profile" ]]; then
  DEFAULT_PROFILE="${ROOT}/pw/profile"
fi

BROWSER_BIN="${ITCY_BROWSER_EXECUTABLE:-${DEFAULT_BIN}}"
if [[ ! -x "${BROWSER_BIN}" ]]; then
  echo "local browser missing or not executable: ${BROWSER_BIN}" >&2
  exit 1
fi

unset PLAYWRIGHT_BROWSERS_PATH
# Prefer host / nvm browsers unless ITCY_PLAYWRIGHT_BROWSERS_PATH is set by the caller.
if [[ -n "${ITCY_PLAYWRIGHT_BROWSERS_PATH:-}" ]]; then
  export PLAYWRIGHT_BROWSERS_PATH="${ITCY_PLAYWRIGHT_BROWSERS_PATH}"
fi
export npm_config_loglevel=error

PW_SCREENSHOTS_DIR="${ITCY_PW_SCREENSHOTS_DIR:-${ITCY_PW_DEBUG_DIR:-${ROOT}/pw/screenshots}}"
mkdir -p "${PW_SCREENSHOTS_DIR}"

PW_MCP_DIR="${ITCY_PW_MCP_DIR:-${ROOT}/pw/mcp}"
mkdir -p "${PW_MCP_DIR}"

PW_PROFILE_DIR="${ITCY_PW_USER_DATA_DIR:-${DEFAULT_PROFILE}}"
mkdir -p "${PW_PROFILE_DIR}"

echo "playwright-mcp: browser=${BROWSER_NAME} bin=${BROWSER_BIN} profile=${PW_PROFILE_DIR} output=${PW_MCP_DIR}" >&2

cd "${ROOT}"
exec "${NPX}" -y @playwright/mcp@latest \
  --executable-path "${BROWSER_BIN}" \
  --sandbox \
  --headless \
  --user-data-dir "${PW_PROFILE_DIR}" \
  --output-dir "${PW_MCP_DIR}" \
  --config "${CONFIG}" \
  "$@"
