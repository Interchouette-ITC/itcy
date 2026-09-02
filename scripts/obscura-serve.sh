#!/usr/bin/env bash
# Obscura CDP server for Lane C ingest (ITCY_PW_BROWSER=obscura).
# Usage:
#   scripts/obscura-serve.sh          # foreground serve
#   scripts/obscura-serve.sh ensure   # start background if down
#   scripts/obscura-serve.sh stop     # stop background serve
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ITCY_ROOT="${ROOT}"
# shellcheck source=scripts/lib/obscura-serve.sh
source "${ROOT}/scripts/lib/obscura-serve.sh"

CMD="${1:-run}"
case "${CMD}" in
  ensure)
    obscura_ensure_serve
    ;;
  stop)
    obscura_stop_serve
    ;;
  run)
    BIN="$(obscura_resolve_bin)"
    PORT="$(obscura_cdp_port)"
    BINDIR="$(cd "$(dirname "${BIN}")" && pwd)"
    ARGS=(serve --port "${PORT}")
    if [[ "${ITCY_OBSCURA_STEALTH:-0}" == "1" ]]; then
      ARGS+=(--stealth)
    fi
    cd "${BINDIR}"
    exec "${BIN}" "${ARGS[@]}"
    ;;
  *)
    echo "usage: $0 [run|ensure|stop]" >&2
    exit 1
    ;;
esac
