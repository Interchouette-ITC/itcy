# Copyright (c) 2026 Interchouette-ITC
# SPDX-License-Identifier: BUSL-1.1
#
# Obscura CDP sidecar helpers for Lane C (fetch-public-page.sh).
# shellcheck shell=bash

obscura_product_root() {
  if [[ -n "${ITCY_ROOT:-}" ]]; then
    echo "${ITCY_ROOT}"
    return 0
  fi
  local lib
  lib="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  echo "$(cd "${lib}/../.." && pwd)"
}

obscura_cdp_port() {
  echo "${ITCY_OBSCURA_CDP_PORT:-9222}"
}

obscura_cdp_base_url() {
  echo "http://127.0.0.1:$(obscura_cdp_port)"
}

obscura_state_dir() {
  local root
  root="$(obscura_product_root)"
  echo "${root}/pw/obscura"
}

obscura_pid_file() {
  echo "$(obscura_state_dir)/serve.pid"
}

obscura_log_file() {
  echo "$(obscura_state_dir)/serve.log"
}

obscura_resolve_bin() {
  if [[ -n "${ITCY_BROWSER_EXECUTABLE:-}" && -x "${ITCY_BROWSER_EXECUTABLE}" ]]; then
    echo "${ITCY_BROWSER_EXECUTABLE}"
    return 0
  fi
  local root default
  root="$(obscura_product_root)"
  default="${root}/tools/obscura/obscura"
  if [[ -x "${default}" ]]; then
    echo "${default}"
    return 0
  fi
  echo "obscura binary missing (set ITCY_BROWSER_EXECUTABLE or install under tools/obscura/obscura)" >&2
  return 1
}

obscura_serve_healthy() {
  curl -fsS --connect-timeout 0.5 "$(obscura_cdp_base_url)/json/version" >/dev/null 2>&1
}

obscura_stale_pid_running() {
  local pid_file pid
  pid_file="$(obscura_pid_file)"
  [[ -f "${pid_file}" ]] || return 1
  pid="$(tr -dc '0-9' <"${pid_file}" 2>/dev/null || true)"
  [[ -n "${pid}" ]] || return 1
  kill -0 "${pid}" 2>/dev/null
}

obscura_clear_stale_pid() {
  local pid_file
  pid_file="$(obscura_pid_file)"
  if [[ -f "${pid_file}" ]] && ! obscura_stale_pid_running; then
    rm -f "${pid_file}"
  fi
}

obscura_ensure_serve() {
  if obscura_serve_healthy; then
    return 0
  fi
  obscura_clear_stale_pid
  local bin root port state log pid_file bindir
  bin="$(obscura_resolve_bin)" || return 1
  root="$(obscura_product_root)"
  port="$(obscura_cdp_port)"
  state="$(obscura_state_dir)"
  log="$(obscura_log_file)"
  pid_file="$(obscura_pid_file)"
  mkdir -p "${state}"
  bindir="$(cd "$(dirname "${bin}")" && pwd)"
  if [[ ! -x "${bindir}/obscura-worker" ]]; then
    echo "obscura-worker missing next to ${bin} (release archive keeps both in one dir)" >&2
    return 1
  fi
  local -a serve_args=(serve --port "${port}")
  if [[ "${ITCY_OBSCURA_STEALTH:-0}" == "1" ]]; then
    serve_args+=(--stealth)
  fi
  (
    cd "${bindir}"
    nohup "${bin}" "${serve_args[@]}" >>"${log}" 2>&1 &
    echo $! >"${pid_file}"
  )
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if obscura_serve_healthy; then
      return 0
    fi
    sleep 0.5
  done
  echo "obscura serve did not become healthy on port ${port} (see ${log})" >&2
  return 1
}

obscura_stop_serve() {
  obscura_clear_stale_pid
  local pid_file pid
  pid_file="$(obscura_pid_file)"
  [[ -f "${pid_file}" ]] || return 0
  pid="$(tr -dc '0-9' <"${pid_file}" 2>/dev/null || true)"
  if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
    kill "${pid}" 2>/dev/null || true
    sleep 0.5
    kill -0 "${pid}" 2>/dev/null && kill -9 "${pid}" 2>/dev/null || true
  fi
  rm -f "${pid_file}"
}
