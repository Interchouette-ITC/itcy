# Copyright (c) 2026 Interchouette-ITC
# SPDX-License-Identifier: BUSL-1.1
#
# Shared by post-twitter / fetch-twitter-pulse / fetch-twitter-status.
# One Brave CDP session at a time; pick a free debugging port (never steal :9224).
# shellcheck shell=bash

# True when no process holds flock on the lock file (safe to remove orphan file).
twitter_brave_lock_unheld() {
  local lock="${1:?}"
  [[ -f "${lock}" ]] || return 0
  exec 8>>"${lock}"
  if flock -n 8; then
    flock -u 8
    exec 8>&- || true
    return 0
  fi
  exec 8>&- || true
  return 1
}

twitter_brave_stale_lock_pid() {
  local lock="${1:?}"
  [[ -f "${lock}" ]] || return 1
  local pid
  pid="$(tr -dc '0-9' <"${lock}" 2>/dev/null || true)"
  [[ -n "${pid}" ]] || return 1
  kill -0 "${pid}" 2>/dev/null
}

twitter_brave_clear_stale_lock() {
  local lock="${1:?}"
  # Never unlink while another process holds flock (PID file can be empty mid-acquire).
  if twitter_brave_lock_unheld "${lock}"; then
    rm -f "${lock}"
    return 0
  fi
  if [[ -f "${lock}" ]] && ! twitter_brave_stale_lock_pid "${lock}"; then
    # Holder PID died but flock should be gone; only clear if flock is also free.
    if twitter_brave_lock_unheld "${lock}"; then
      rm -f "${lock}"
    fi
  fi
}

twitter_brave_acquire_lock() {
  local run_root="${1:?run root}"
  local wait_secs="${2:-300}"
  mkdir -p "${run_root}"
  local lock="${run_root}/brave.lock"
  local deadline=$((SECONDS + wait_secs))
  while (( SECONDS < deadline )); do
    twitter_brave_clear_stale_lock "${lock}"
    exec 9<>"${lock}"
    if flock -n 9; then
      : >"${lock}"
      echo "$$" >&9
      return 0
    fi
    exec 9>&- || true
    sleep 2
  done
  local holder=""
  if [[ -f "${lock}" ]]; then
    holder="$(tr -dc '0-9' <"${lock}" 2>/dev/null || true)"
  fi
  if [[ -n "${holder}" ]]; then
    echo "another X Brave session holds ${lock} (pid ${holder}); wait or stop the other ship/pulse/status run" >&2
  else
    echo "another X Brave session holds ${lock}; wait or stop the other ship/pulse/status run" >&2
  fi
  return 1
}

twitter_brave_pick_cdp_port() {
  local preferred="${1:-}"
  local p
  if [[ -n "${preferred}" ]]; then
    if twitter_brave_port_free "${preferred}"; then
      echo "${preferred}"
      return 0
    fi
  fi
  for p in $(seq 9230 9299); do
    if twitter_brave_port_free "${p}"; then
      echo "${p}"
      return 0
    fi
  done
  echo "no free X CDP port in 9230-9299" >&2
  return 1
}

twitter_brave_port_free() {
  local port="${1:?}"
  if curl -fsS --connect-timeout 0.2 "http://127.0.0.1:${port}/json/version" >/dev/null 2>&1; then
    return 1
  fi
  if command -v ss >/dev/null 2>&1; then
    if ss -tln | grep -qE ":${port}\\s"; then
      return 1
    fi
  fi
  return 0
}

twitter_brave_cdp_ready() {
  local port="${1:?}"
  curl -fsS --connect-timeout 0.3 "http://127.0.0.1:${port}/json/version" >/dev/null 2>&1
}
