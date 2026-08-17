#!/usr/bin/env bash
# Tor-up + LinkedIn URL enrich drip (standalone binary).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

bash "${ROOT}/scripts/tor-up.sh"

export ITCY_STATE_DB="${ITCY_STATE_DB:-${ROOT}/sql/runtime.db}"
export ITCY_SCRAPE_CACHE_DB="${ITCY_SCRAPE_CACHE_DB:-${ROOT}/sql/linkedin-scrape-cache.db}"
export ITCY_TOR_SOCKS="${ITCY_TOR_SOCKS:-socks5h://127.0.0.1:9050}"
export ITCY_TOR_CONTROL="${ITCY_TOR_CONTROL:-127.0.0.1:9051}"

# Always rebuild (cargo is incremental). Stale release binaries caused false "LinkedIn wall"
# panics (UTF-8 byte indexing) when source was already fixed in the tree.
mkdir -p "${ROOT}/sql/tmp"
export TMPDIR="${TMPDIR:-${ROOT}/sql/tmp}"
target_dir="${ROOT}/backend/target"
echo "building enrich-linkedin-urls (release)…"
(cd "${ROOT}/backend" && cargo build --release -p itcy --bin enrich-linkedin-urls --target-dir "${target_dir}")

enrich_bin="${target_dir}/release/enrich-linkedin-urls"
if [[ ! -x "${enrich_bin}" ]]; then
  echo "missing ${enrich_bin} after cargo build" >&2
  exit 1
fi
exec "${enrich_bin}" --loop "$@"
