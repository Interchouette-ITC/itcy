#!/usr/bin/env bash
# Lane C A/B: fetch-public-page baseline vs ITCY_PW_BROWSER=obscura.
# Usage: scripts/obscura-parity-harness.sh
# Exit 0 when every URL matches on normalized body text; non-zero on regression.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FETCH="${ROOT}/scripts/fetch-public-page.sh"
HARNESS_PORT="${ITCY_OBSCURA_CDP_PORT:-9227}"
OUT_DIR="${ROOT}/pw/obscura/parity-$(date +%Y%m%dT%H%M%S)"
mkdir -p "${OUT_DIR}"

if [[ ! -x "${ROOT}/tools/obscura/obscura" ]]; then
  echo "missing ${ROOT}/tools/obscura/obscura; install Obscura release before parity run" >&2
  exit 1
fi

URLS=(
  "https://example.com"
  "https://news.ycombinator.com"
  "http://example.com"
)

normalize_html() {
  local file="${1:?}"
  node --input-type=module -e "
const fs = await import('node:fs');
const html = fs.readFileSync(process.argv[1], 'utf8');
const text = html
  .replace(/<script[\\s\\S]*?<\\/script>/gi, ' ')
  .replace(/<style[\\s\\S]*?<\\/style>/gi, ' ')
  .replace(/<[^>]+>/g, ' ')
  .replace(/\\s+/g, ' ')
  .trim();
process.stdout.write(text);
" "${file}"
}

run_fetch() {
  local label="${1:?}"
  local url="${2:?}"
  local out="${OUT_DIR}/${label}.html"
  local -a env_args=()
  if [[ "${label}" == obscura ]]; then
    env_args=(
      ITCY_PW_BROWSER=obscura
      ITCY_OBSCURA_CDP_PORT="${HARNESS_PORT}"
      ITCY_BROWSER_EXECUTABLE="${ROOT}/tools/obscura/obscura"
    )
  fi
  local start end elapsed
  start="$(date +%s%N)"
  if ! env "${env_args[@]}" bash "${FETCH}" "${url}" >"${out}" 2>"${OUT_DIR}/${label}.err"; then
    echo "FAIL ${label} fetch ${url} (see ${OUT_DIR}/${label}.err)" >&2
    return 1
  fi
  end="$(date +%s%N)"
  elapsed=$(( (end - start) / 1000000 ))
  local chars hash
  chars="$(wc -c <"${out}" | tr -d ' ')"
  hash="$(sha256sum "${out}" | awk '{print $1}')"
  normalize_html "${out}" >"${OUT_DIR}/${label}.txt"
  local text_chars text_hash
  text_chars="$(wc -c <"${OUT_DIR}/${label}.txt" | tr -d ' ')"
  text_hash="$(sha256sum "${OUT_DIR}/${label}.txt" | awk '{print $1}')"
  echo "${label} url=${url} html_bytes=${chars} html_sha=${hash} text_bytes=${text_chars} text_sha=${text_hash} ms=${elapsed}"
}

failures=0
for url in "${URLS[@]}"; do
  slug="${url#https://}"
  slug="${slug#http://}"
  slug="${slug//\//_}"
  slug="${slug//:/_}"
  slug="${slug//./_}"
  echo "== ${url} =="
  unset ITCY_PW_BROWSER ITCY_BROWSER_EXECUTABLE ITCY_OBSCURA_CDP_PORT
  base_line="$(run_fetch "baseline-${slug}" "${url}")" || failures=$((failures + 1))
  obs_line="$(run_fetch "obscura-${slug}" "${url}")" || failures=$((failures + 1))
  echo "${base_line}"
  echo "${obs_line}"
  base_txt="${OUT_DIR}/baseline-${slug}.txt"
  obs_txt="${OUT_DIR}/obscura-${slug}.txt"
  if [[ -f "${base_txt}" && -f "${obs_txt}" ]]; then
    if ! cmp -s "${base_txt}" "${obs_txt}"; then
      echo "TEXT MISMATCH ${url} (see ${OUT_DIR})" >&2
      failures=$((failures + 1))
    else
      echo "TEXT OK ${url}"
    fi
  fi
done

# Stop harness-only sidecar if we started it on a non-default port.
ITCY_OBSCURA_CDP_PORT="${HARNESS_PORT}" ITCY_ROOT="${ROOT}" bash "${ROOT}/scripts/obscura-serve.sh" stop 2>/dev/null || true

if (( failures > 0 )); then
  echo "parity harness: ${failures} failure(s); artifacts ${OUT_DIR}" >&2
  exit 1
fi
echo "parity harness: pass; artifacts ${OUT_DIR}"
