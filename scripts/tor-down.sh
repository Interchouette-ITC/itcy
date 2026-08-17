#!/usr/bin/env bash
# Stop ITCy Tor daemon started by tor-up.sh.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PID_FILE="${ROOT}/sql/tor.pid"

if [[ ! -f "${PID_FILE}" ]]; then
  echo "no tor pid file"
  exit 0
fi
pid="$(cat "${PID_FILE}")"
if [[ -n "${pid}" ]] && [[ -d "/proc/${pid}" ]]; then
  kill "${pid}" || true
  sleep 1
  if [[ -d "/proc/${pid}" ]]; then
    kill -9 "${pid}" || true
  fi
  echo "tor stopped (was pid ${pid})"
else
  echo "tor not running"
fi
rm -f "${PID_FILE}"
