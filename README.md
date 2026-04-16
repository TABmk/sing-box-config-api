# sing-box-config-api

Small HTTP API for managing a `sing-box` config and service on Linux

Designed to work as a backend for [this web panel](https://github.com/TABmk/sing-box-config-api-panel)

## Features

- Check `sing-box` daemon status
- Read and update `sing-box` config JSON
- Run `sing-box check`
- Restart the `sing-box` service
- Create and restore config backups
- List available `.srs` files
- Download `.srs` files from GitHub links

## API

The API listens on port `17118` and uses the `/api` prefix.

Base URL example:

```text
http://<host>:17118/api
```

All routes require a secret.

You can pass it with either:

- `x-api-secret: <secret>`
- `Authorization: Bearer <secret>`

## Routes

- `GET /api/health`
- `GET /api/status`
- `GET /api/config`
- `PUT /api/config`
- `POST /api/check`
- `POST /api/restart`
- `GET /api/backups`
- `POST /api/backups`
- `POST /api/backups/:name/restore`
- `GET /api/srs`
- `POST /api/srs/download`

## Config

The app loads `config.toml` from the same directory as the binary.

If no config file exists, built-in defaults are used.

Important:

- default secret is `changeme`
- the app refuses to start if the secret is still `changeme`

Example config:

```toml
secret = "replace-with-strong-secret"
listen_addr = "0.0.0.0:17118"
sing_box_config_path = "/etc/sing-box/config.json"
backups_dir = "/etc/sing-box/backups"
srs_dir = "/etc/sing-box/srs"
status_command = "sudo -n /usr/local/libexec/sing-box-config-api/status"
check_command = "sudo -n /usr/local/libexec/sing-box-config-api/check {config_path}"
restart_command = "sudo -n /usr/local/libexec/sing-box-config-api/restart"
```

## Build

```bash
cargo build --release
```

Binary:

```text
target/release/sing-box-config-api
```

## Install

The repo includes an installer:

```bash
sudo bash install.sh
```

It will:

- install the binary to `/opt/sing-box-config-api`
- install runtime config to `/opt/sing-box-config-api/config.toml`
- create a dedicated `singbox-api` service user
- install a `systemd` service
- install restricted `sudo` wrappers for `status`, `check`, and `restart`
- prepare `/etc/sing-box/config.json` and `/etc/sing-box/backups`
- prepare `/etc/sing-box/srs`

## Service Control

After install:

```bash
sing-box-config-api-start
sing-box-config-api-stop
```

You can also use `systemctl` directly:

```bash
sudo systemctl status sing-box-config-api.service
sudo systemctl restart sing-box-config-api.service
```

## Logs

```bash
sudo journalctl -u sing-box-config-api.service -f
```

## Example Requests

Health:

```bash
curl -H 'x-api-secret: YOUR_SECRET' \
  http://127.0.0.1:17118/api/health
```

Get config:

```bash
curl -H 'x-api-secret: YOUR_SECRET' \
  http://127.0.0.1:17118/api/config
```

Run check:

```bash
curl -X POST \
  -H 'x-api-secret: YOUR_SECRET' \
  -H 'content-type: application/json' \
  -d '{"config":{"log":{"level":"info"}}}' \
  http://127.0.0.1:17118/api/check
```

Restart service:

```bash
curl -X POST -H 'x-api-secret: YOUR_SECRET' \
  http://127.0.0.1:17118/api/restart
```

Restore backup:

```bash
curl -X POST -H 'x-api-secret: YOUR_SECRET' \
  http://127.0.0.1:17118/api/backups/config-YYYYMMDD-HHMMSS-000.json/restore
```

List `.srs` files:

```bash
curl -H 'x-api-secret: YOUR_SECRET' \
  http://127.0.0.1:17118/api/srs
```

Download `.srs` file from GitHub:

```bash
curl -X POST \
  -H 'x-api-secret: YOUR_SECRET' \
  -H 'content-type: application/json' \
  -d '{"url":"https://github.com/KaringX/karing-ruleset/raw/refs/heads/sing/russia/antizapret/antizapret.srs"}' \
  http://127.0.0.1:17118/api/srs/download
```
