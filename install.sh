#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${APP_NAME:-sing-box-config-api}"
SERVICE_NAME="${SERVICE_NAME:-${APP_NAME}.service}"
APP_USER="${APP_USER:-singbox-api}"
APP_GROUP="${APP_GROUP:-${APP_USER}}"
INSTALL_DIR="${INSTALL_DIR:-/opt/${APP_NAME}}"
LIBEXEC_DIR="${LIBEXEC_DIR:-/usr/local/libexec/${APP_NAME}}"
RUN_CONFIG_PATH="${RUN_CONFIG_PATH:-${INSTALL_DIR}/config.toml}"
SUDOERS_PATH="${SUDOERS_PATH:-/etc/sudoers.d/${APP_NAME}}"
UNIT_PATH="${UNIT_PATH:-/etc/systemd/system/${SERVICE_NAME}}"
SB_CONFIG_PATH="${SB_CONFIG_PATH:-/etc/sing-box/config.json}"
SB_BACKUPS_DIR="${SB_BACKUPS_DIR:-/etc/sing-box/backups}"
SRS_DIR="${SRS_DIR:-/etc/sing-box/srs}"
START_SERVICE="${START_SERVICE:-1}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_CONFIG="${SCRIPT_DIR}/config.toml"
SOURCE_BINARY="${SOURCE_BINARY:-}"

if [[ -z "$SOURCE_BINARY" ]]; then
  if [[ -x "${SCRIPT_DIR}/${APP_NAME}" ]]; then
    SOURCE_BINARY="${SCRIPT_DIR}/${APP_NAME}"
  else
    SOURCE_BINARY="${SCRIPT_DIR}/target/release/${APP_NAME}"
  fi
fi

ensure_root() {
  if [[ "$(id -u)" -ne 0 ]]; then
    echo "run this installer as root: sudo ./install.sh" >&2
    exit 1
  fi
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "required command not found: $1" >&2
    exit 1
  fi
}

random_secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
  else
    tr -dc 'A-Za-z0-9' </dev/urandom | head -c 48
    printf '\n'
  fi
}

ensure_group() {
  if ! getent group "$APP_GROUP" >/dev/null 2>&1; then
    groupadd --system "$APP_GROUP"
  fi
}

ensure_user() {
  if ! id -u "$APP_USER" >/dev/null 2>&1; then
    useradd \
      --system \
      --gid "$APP_GROUP" \
      --home-dir "$INSTALL_DIR" \
      --no-create-home \
      --shell /usr/sbin/nologin \
      "$APP_USER"
  fi
}

install_binary() {
  install -d -o root -g root -m 0755 "$INSTALL_DIR"
  install -o root -g root -m 0755 "$SOURCE_BINARY" "${INSTALL_DIR}/${APP_NAME}"
}

set_toml_string() {
  local file="$1"
  local key="$2"
  local value="$3"
  local escaped

  escaped="$(printf '%s' "$value" | sed 's/[\/&|]/\\&/g')"
  if grep -Eq "^[[:space:]]*${key}[[:space:]]*=" "$file"; then
    sed -i "s|^[[:space:]]*${key}[[:space:]]*=.*|${key} = \"${escaped}\"|" "$file"
  else
    printf '%s = "%s"\n' "$key" "$value" >>"$file"
  fi
}

install_runtime_config() {
  local created_config=0
  local generated_secret

  if [[ ! -f "$SOURCE_CONFIG" ]]; then
    echo "missing source config template: $SOURCE_CONFIG" >&2
    exit 1
  fi

  if [[ ! -f "$RUN_CONFIG_PATH" ]]; then
    install -o root -g "$APP_GROUP" -m 0640 "$SOURCE_CONFIG" "$RUN_CONFIG_PATH"
    created_config=1
  else
    chown root:"$APP_GROUP" "$RUN_CONFIG_PATH"
    chmod 0640 "$RUN_CONFIG_PATH"
  fi

  set_toml_string "$RUN_CONFIG_PATH" "status_command" "sudo -n ${LIBEXEC_DIR}/status"
  set_toml_string "$RUN_CONFIG_PATH" "check_command" "sudo -n ${LIBEXEC_DIR}/check {config_path}"
  set_toml_string "$RUN_CONFIG_PATH" "restart_command" "sudo -n ${LIBEXEC_DIR}/restart"
  set_toml_string "$RUN_CONFIG_PATH" "sing_box_config_path" "$SB_CONFIG_PATH"
  set_toml_string "$RUN_CONFIG_PATH" "backups_dir" "$SB_BACKUPS_DIR"
  set_toml_string "$RUN_CONFIG_PATH" "srs_dir" "$SRS_DIR"

  if [[ "$created_config" == "1" ]]; then
    generated_secret="$(random_secret)"
    set_toml_string "$RUN_CONFIG_PATH" "secret" "$generated_secret"
    echo "generated API secret: $generated_secret"
  elif grep -Eq '^[[:space:]]*secret[[:space:]]*=[[:space:]]*"changeme"' "$RUN_CONFIG_PATH"; then
    generated_secret="$(random_secret)"
    set_toml_string "$RUN_CONFIG_PATH" "secret" "$generated_secret"
    echo "generated API secret: $generated_secret"
  fi
}

install_wrappers() {
  install -d -o root -g root -m 0755 "$LIBEXEC_DIR"

  install -o root -g root -m 0755 /dev/null "${LIBEXEC_DIR}/status"
  install -o root -g root -m 0755 /dev/null "${LIBEXEC_DIR}/check"
  install -o root -g root -m 0755 /dev/null "${LIBEXEC_DIR}/restart"

  cat >"${LIBEXEC_DIR}/status" <<EOF
#!/bin/sh
exec /usr/bin/systemctl status sing-box --no-pager
EOF

  cat >"${LIBEXEC_DIR}/check" <<EOF
#!/bin/sh
CONFIG_PATH="\${1:-${SB_CONFIG_PATH}}"
exec /usr/bin/sing-box check -c "\${CONFIG_PATH}"
EOF

  cat >"${LIBEXEC_DIR}/restart" <<EOF
#!/bin/sh
exec /usr/bin/systemctl restart sing-box
EOF

  chown root:root "${LIBEXEC_DIR}/status" "${LIBEXEC_DIR}/check" "${LIBEXEC_DIR}/restart"
  chmod 0755 "${LIBEXEC_DIR}/status" "${LIBEXEC_DIR}/check" "${LIBEXEC_DIR}/restart"
}

install_sudoers() {
  install -o root -g root -m 0440 /dev/null "$SUDOERS_PATH"
  cat >"$SUDOERS_PATH" <<EOF
${APP_USER} ALL=(root) NOPASSWD: ${LIBEXEC_DIR}/status
${APP_USER} ALL=(root) NOPASSWD: ${LIBEXEC_DIR}/check
${APP_USER} ALL=(root) NOPASSWD: ${LIBEXEC_DIR}/restart
EOF
  chmod 0440 "$SUDOERS_PATH"
  visudo -cf "$SUDOERS_PATH" >/dev/null
}

prepare_sing_box_paths() {
  install -d -o root -g root -m 0755 "$(dirname "$SB_CONFIG_PATH")"

  if [[ ! -f "$SB_CONFIG_PATH" ]]; then
    install -o root -g "$APP_GROUP" -m 0660 /dev/null "$SB_CONFIG_PATH"
    printf '{}\n' >"$SB_CONFIG_PATH"
    chown root:"$APP_GROUP" "$SB_CONFIG_PATH"
    chmod 0660 "$SB_CONFIG_PATH"
  else
    chown root:"$APP_GROUP" "$SB_CONFIG_PATH"
    chmod 0660 "$SB_CONFIG_PATH"
  fi

  install -d -o root -g "$APP_GROUP" -m 2770 "$SB_BACKUPS_DIR"
  chown root:"$APP_GROUP" "$SB_BACKUPS_DIR"
  chmod 2770 "$SB_BACKUPS_DIR"

  if [[ -d "$SB_BACKUPS_DIR" ]]; then
    find "$SB_BACKUPS_DIR" -type d -exec chmod 2770 {} +
    find "$SB_BACKUPS_DIR" -type f -exec chmod 0660 {} +
    chgrp -R "$APP_GROUP" "$SB_BACKUPS_DIR"
  fi

  install -d -o root -g "$APP_GROUP" -m 2770 "$SRS_DIR"
  chown root:"$APP_GROUP" "$SRS_DIR"
  chmod 2770 "$SRS_DIR"

  if [[ -d "$SRS_DIR" ]]; then
    find "$SRS_DIR" -type d -exec chmod 2770 {} +
    find "$SRS_DIR" -type f -name '*.srs' -exec chmod 0660 {} +
    chgrp -R "$APP_GROUP" "$SRS_DIR"
  fi
}

install_unit() {
  install -o root -g root -m 0644 /dev/null "$UNIT_PATH"
  cat >"$UNIT_PATH" <<EOF
[Unit]
Description=Sing-box config backend API
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${APP_USER}
Group=${APP_GROUP}
WorkingDirectory=${INSTALL_DIR}
ExecStart=${INSTALL_DIR}/${APP_NAME}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
}

install_helper_scripts() {
  install -d -o root -g root -m 0755 /usr/local/bin
  install -o root -g root -m 0755 "${SCRIPT_DIR}/scripts/start-service.sh" /usr/local/bin/${APP_NAME}-start
  install -o root -g root -m 0755 "${SCRIPT_DIR}/scripts/stop-service.sh" /usr/local/bin/${APP_NAME}-stop
}

reload_systemd() {
  systemctl daemon-reload
  systemctl enable "$SERVICE_NAME"

  if [[ "$START_SERVICE" == "1" ]]; then
    systemctl restart "$SERVICE_NAME"
    systemctl --no-pager --full status "$SERVICE_NAME"
  else
    echo "service installed but not started because START_SERVICE=${START_SERVICE}"
  fi
}

main() {
  ensure_root
  require_cmd systemctl
  require_cmd install
  require_cmd getent
  require_cmd find
  require_cmd sudo
  require_cmd visudo

  if [[ ! -x "$SOURCE_BINARY" ]]; then
    echo "missing built binary: $SOURCE_BINARY" >&2
    exit 1
  fi

  ensure_group
  ensure_user
  install_binary
  install_runtime_config
  install_wrappers
  install_sudoers
  prepare_sing_box_paths
  install_unit
  install_helper_scripts
  reload_systemd

  echo
  echo "installed ${APP_NAME}"
  echo "binary: ${INSTALL_DIR}/${APP_NAME}"
  echo "config: ${RUN_CONFIG_PATH}"
  echo "service: ${SERVICE_NAME}"
  echo "helper commands: ${APP_NAME}-start, ${APP_NAME}-stop"
}

main "$@"
