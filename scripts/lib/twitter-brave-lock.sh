# Copyright (c) 2026 Interchouette-ITC
# SPDX-License-Identifier: BUSL-1.1
#
# Shared by post-twitter / fetch-twitter-pulse / fetch-twitter-status.
# One Brave CDP session at a time; pick a free debugging port (never steal :9224).
# shellcheck shell=bash

twitter_brave_acquire_lock() {
  local run_root="${1:?run root}"
  mkdir -p "${run_root}"
  local lock="${run_root}/brave.lock"
  exec 9>"${lock}"
  if ! flock -n 9; then
    echo "another X Brave session holds ${lock}; wait or stop the other ship/pulse/status run" >&2
    return 1
  fi
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
