#!/usr/bin/env bash
# Start ITCy Tor daemon (SOCKS 9050 + control 9051). Set ITCY_TOR_BIN to the tor binary.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -z "${ITCY_TOR_BIN:-}" && -f "${ROOT}/.env" ]]; then
  set -a
  # shellcheck source=/dev/null
  . "${ROOT}/.env"
  set +a
fi

TOR_BIN="${ITCY_TOR_BIN:-}"
if [[ -z "${TOR_BIN}" ]]; then
  echo "set ITCY_TOR_BIN to your tor binary path" >&2
  exit 1
fi
PID_FILE="${ROOT}/sql/tor.pid"
DATA_DIR="${ROOT}/sql/tor-data"
TORRC="${ROOT}/docker/torrc"
RUN_TORRC="${DATA_DIR}/torrc.itcy"

if [[ ! -x "${TOR_BIN}" ]]; then
  echo "tor binary missing or not executable: ${TOR_BIN}" >&2
  echo "set ITCY_TOR_BIN to an executable tor binary" >&2
  exit 1
fi

mkdir -p "${DATA_DIR}"
sed "s|ROOT_PLACEHOLDER|${ROOT}|g" "${TORRC}" > "${RUN_TORRC}"

if [[ -f "${PID_FILE}" ]]; then
  old="$(cat "${PID_FILE}" || true)"
  if [[ -n "${old}" ]] && [[ -d "/proc/${old}" ]]; then
    echo "tor already up (pid ${old})"
    exit 0
  fi
  rm -f "${PID_FILE}"
fi

# Fail if something else already owns 9050.
if ss -ltn 2>/dev/null | grep -q ':9050'; then
  if ! curl -sf --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip >/dev/null 2>&1; then
    echo "port 9050 in use but Tor check failed" >&2
    exit 1
  fi
  echo "SOCKS 9050 already accepting Tor traffic"
  exit 0
fi

# Daemonize quietly: notices go to DataDirectory/notice.log (see docker/torrc).
# Without redirect, bundled Tor still prints bootstrap lines on the parent tty.
"${TOR_BIN}" -f "${RUN_TORRC}" --RunAsDaemon 1 --PidFile "${PID_FILE}" \
  >/dev/null 2>&1
echo "waiting for SOCKS 9050…"
for _ in $(seq 1 60); do
  if ss -ltn 2>/dev/null | grep -q ':9050'; then
    echo "tor up (SOCKS 9050, Control 9051, pid $(cat "${PID_FILE}" 2>/dev/null || echo '?'))"
    exit 0
  fi
  sleep 1
done
echo "tor failed to open SOCKS 9050" >&2
if [[ -f "${DATA_DIR}/notice.log" ]]; then
  echo "--- last notice.log ---" >&2
  tail -n 20 "${DATA_DIR}/notice.log" >&2 || true
fi
exit 1
