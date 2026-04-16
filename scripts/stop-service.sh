#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="${SERVICE_NAME:-sing-box-config-api.service}"

run_systemctl() {
  if [[ "$(id -u)" -ne 0 ]]; then
    sudo systemctl "$@"
  else
    systemctl "$@"
  fi
}

run_systemctl stop "$SERVICE_NAME"
run_systemctl --no-pager --full status "$SERVICE_NAME" || true
