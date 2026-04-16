use crate::{AppState, config::describe_config_source};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env,
    path::{Path as StdPath, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{fs, process::Command};

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    listen_addr: String,
    config_source: String,
    sing_box_config_path: String,
    backups_dir: String,
    srs_dir: String,
}

#[derive(Debug, Serialize)]
struct CommandResponse {
    command: String,
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize)]
struct ConfigFileResponse {
    path: String,
    raw: String,
    json: Option<Value>,
    parse_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateConfigRequest {
    config: Value,
}

#[derive(Debug, Serialize)]
struct UpdateConfigResponse {
    path: String,
    backup: Option<BackupEntry>,
}

#[derive(Debug, Serialize)]
struct BackupEntry {
    name: String,
    path: String,
    size_bytes: u64,
    modified_at: String,
}

#[derive(Debug, Serialize)]
struct BackupsResponse {
    backups: Vec<BackupEntry>,
}

#[derive(Debug, Serialize)]
struct RestoreBackupResponse {
    restored_from: BackupEntry,
    previous_config_backup: Option<BackupEntry>,
}

#[derive(Debug, Serialize)]
struct SrsEntry {
    name: String,
    path: String,
    size_bytes: u64,
    modified_at: String,
}

#[derive(Debug, Serialize)]
struct SrsFilesResponse {
    files: Vec<SrsEntry>,
}

#[derive(Debug, Deserialize)]
struct DownloadSrsRequest {
    url: String,
}

#[derive(Debug, Serialize)]
struct DownloadSrsResponse {
    source_url: String,
    file: SrsEntry,
}

#[derive(Debug)]
struct BackupCandidate {
    entry: BackupEntry,
    sort_key: u128,
}

#[derive(Debug)]
struct SrsCandidate {
    entry: SrsEntry,
    sort_key: u128,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub fn api_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/config", get(get_config).put(update_config))
        .route("/check", post(run_check))
        .route("/restart", post(restart_service))
        .route("/backups", get(list_backups).post(create_backup))
        .route("/backups/:name/restore", post(restore_backup))
        .route("/srs", get(list_srs_files))
        .route("/srs/download", post(download_srs_file))
        .layer(middleware::from_fn_with_state(state, require_secret))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        listen_addr: state.runtime_config.settings.listen_addr.clone(),
        config_source: describe_config_source(state.runtime_config.config_source.as_deref()),
        sing_box_config_path: state
            .runtime_config
            .settings
            .sing_box_config_path
            .display()
            .to_string(),
        backups_dir: state
            .runtime_config
            .settings
            .backups_dir
            .display()
            .to_string(),
        srs_dir: state.runtime_config.settings.srs_dir.display().to_string(),
    })
}

async fn status(State(state): State<AppState>) -> Result<Json<CommandResponse>, ApiError> {
    Ok(Json(
        run_command(&state.runtime_config.settings.status_command).await?,
    ))
}

async fn get_config(State(state): State<AppState>) -> Result<Json<ConfigFileResponse>, ApiError> {
    let path = &state.runtime_config.settings.sing_box_config_path;
    let raw = fs::read_to_string(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(format!("sing-box config not found: {}", path.display()))
        } else {
            ApiError::internal(format!(
                "failed to read sing-box config {}: {error}",
                path.display()
            ))
        }
    })?;

    let parsed = serde_json::from_str::<Value>(&raw);
    let (json, parse_error) = match parsed {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    };

    Ok(Json(ConfigFileResponse {
        path: path.display().to_string(),
        raw,
        json,
        parse_error,
    }))
}

async fn update_config(
    State(state): State<AppState>,
    Json(payload): Json<UpdateConfigRequest>,
) -> Result<Json<UpdateConfigResponse>, ApiError> {
    let path = &state.runtime_config.settings.sing_box_config_path;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::internal(format!(
                "failed to create config directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let backup = if fs::try_exists(path).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to check config presence {}: {error}",
            path.display()
        ))
    })? {
        Some(create_backup_snapshot(&state).await?)
    } else {
        None
    };

    let content = serde_json::to_string_pretty(&payload.config).map_err(|error| {
        ApiError::bad_request(format!("failed to serialize config payload: {error}"))
    })?;

    fs::write(path, format!("{content}\n"))
        .await
        .map_err(|error| {
            ApiError::internal(format!(
                "failed to write sing-box config {}: {error}",
                path.display()
            ))
        })?;

    Ok(Json(UpdateConfigResponse {
        path: path.display().to_string(),
        backup,
    }))
}

async fn run_check(
    State(state): State<AppState>,
    Json(payload): Json<UpdateConfigRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let temp_path = build_temp_check_path();
    let content = serde_json::to_string_pretty(&payload.config).map_err(|error| {
        ApiError::bad_request(format!("failed to serialize config payload: {error}"))
    })?;

    fs::write(&temp_path, format!("{content}\n"))
        .await
        .map_err(|error| {
            ApiError::internal(format!(
                "failed to write temporary config {}: {error}",
                temp_path.display()
            ))
        })?;

    let command_parts =
        build_check_command_parts(&state.runtime_config.settings.check_command, &temp_path)?;
    let response = run_command_parts(command_parts).await;
    let _ = fs::remove_file(&temp_path).await;

    Ok(Json(response?))
}

async fn restart_service(State(state): State<AppState>) -> Result<Json<CommandResponse>, ApiError> {
    Ok(Json(
        run_command(&state.runtime_config.settings.restart_command).await?,
    ))
}

async fn list_backups(State(state): State<AppState>) -> Result<Json<BackupsResponse>, ApiError> {
    let backups = read_backups(&state).await?;
    Ok(Json(BackupsResponse { backups }))
}

async fn create_backup(State(state): State<AppState>) -> Result<Json<BackupEntry>, ApiError> {
    Ok(Json(create_backup_snapshot(&state).await?))
}

async fn restore_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<RestoreBackupResponse>, ApiError> {
    let backup_name = sanitize_backup_name(&name)?;
    let backup_path = state.runtime_config.settings.backups_dir.join(&backup_name);

    if !fs::try_exists(&backup_path).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to check backup presence {}: {error}",
            backup_path.display()
        ))
    })? {
        return Err(ApiError::not_found(format!(
            "backup not found: {}",
            backup_path.display()
        )));
    }

    let previous_config_backup =
        if fs::try_exists(&state.runtime_config.settings.sing_box_config_path)
            .await
            .map_err(|error| {
                ApiError::internal(format!(
                    "failed to check current config presence {}: {error}",
                    state.runtime_config.settings.sing_box_config_path.display()
                ))
            })?
        {
            Some(create_backup_snapshot(&state).await?)
        } else {
            None
        };

    if let Some(parent) = state.runtime_config.settings.sing_box_config_path.parent() {
        fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::internal(format!(
                "failed to create config directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let backup_content = fs::read(&backup_path).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to read backup {}: {error}",
            backup_path.display()
        ))
    })?;

    fs::write(
        &state.runtime_config.settings.sing_box_config_path,
        backup_content,
    )
    .await
    .map_err(|error| {
        ApiError::internal(format!(
            "failed to restore backup {} to {}: {error}",
            backup_path.display(),
            state.runtime_config.settings.sing_box_config_path.display()
        ))
    })?;

    let restored_from = backup_entry_from_path(&backup_path).await?;

    Ok(Json(RestoreBackupResponse {
        restored_from,
        previous_config_backup,
    }))
}

async fn list_srs_files(State(state): State<AppState>) -> Result<Json<SrsFilesResponse>, ApiError> {
    let files = read_srs_files(&state).await?;
    Ok(Json(SrsFilesResponse { files }))
}

async fn download_srs_file(
    State(state): State<AppState>,
    Json(payload): Json<DownloadSrsRequest>,
) -> Result<Json<DownloadSrsResponse>, ApiError> {
    let (url, file_name) = parse_srs_download_url(&payload.url)?;
    let response = reqwest::get(url.clone()).await.map_err(|error| {
        ApiError::internal(format!("failed to download srs file from {url}: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::internal(format!(
            "failed to download srs file from {url}: upstream returned {status}"
        )));
    }

    let file_bytes = response.bytes().await.map_err(|error| {
        ApiError::internal(format!(
            "failed to read downloaded srs content from {url}: {error}"
        ))
    })?;

    fs::create_dir_all(&state.runtime_config.settings.srs_dir)
        .await
        .map_err(|error| {
            ApiError::internal(format!(
                "failed to create srs directory {}: {error}",
                state.runtime_config.settings.srs_dir.display()
            ))
        })?;

    let file_path = state.runtime_config.settings.srs_dir.join(&file_name);
    fs::write(&file_path, &file_bytes).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to store downloaded srs file {}: {error}",
            file_path.display()
        ))
    })?;

    let file = srs_candidate_from_path(&file_path).await?.entry;

    Ok(Json(DownloadSrsResponse {
        source_url: payload.url,
        file,
    }))
}

async fn require_secret(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided_secret = extract_secret(request.headers());
    if provided_secret != Some(state.runtime_config.settings.secret.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized: provide a valid x-api-secret or Bearer token",
        ));
    }

    Ok(next.run(request).await)
}

fn extract_secret(headers: &HeaderMap) -> Option<&str> {
    if let Some(secret) = headers
        .get("x-api-secret")
        .and_then(|value| value.to_str().ok())
    {
        return Some(secret);
    }

    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

async fn run_command(command_line: &str) -> Result<CommandResponse, ApiError> {
    let parts = split_command(command_line)?;
    run_command_parts(parts).await
}

fn split_command(command_line: &str) -> Result<Vec<String>, ApiError> {
    shell_words::split(command_line).map_err(|error| {
        ApiError::internal(format!("failed to parse command '{command_line}': {error}"))
    })
}

async fn run_command_parts(parts: Vec<String>) -> Result<CommandResponse, ApiError> {
    let display_command = shell_words::join(parts.iter().map(String::as_str));
    let (program, args) = parts
        .split_first()
        .ok_or_else(|| ApiError::internal("configured command is empty"))?;

    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|error| {
            ApiError::internal(format!(
                "failed to execute command '{}': {error}",
                display_command
            ))
        })?;

    Ok(CommandResponse {
        command: display_command,
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn build_check_command_parts(
    command_template: &str,
    config_path: &StdPath,
) -> Result<Vec<String>, ApiError> {
    let config_path = config_path.display().to_string();
    let mut parts = split_command(command_template)?;
    let mut replaced = false;

    for part in &mut parts {
        if part.contains("{config_path}") {
            *part = part.replace("{config_path}", &config_path);
            replaced = true;
        }
    }

    if !replaced {
        parts.push(config_path);
    }

    Ok(parts)
}

fn build_temp_check_path() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    env::temp_dir().join(format!("sing-box-config-api-check-{suffix}.json"))
}

async fn create_backup_snapshot(state: &AppState) -> Result<BackupEntry, ApiError> {
    let source = &state.runtime_config.settings.sing_box_config_path;

    if !fs::try_exists(source).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to check config presence {}: {error}",
            source.display()
        ))
    })? {
        return Err(ApiError::not_found(format!(
            "sing-box config not found: {}",
            source.display()
        )));
    }

    fs::create_dir_all(&state.runtime_config.settings.backups_dir)
        .await
        .map_err(|error| {
            ApiError::internal(format!(
                "failed to create backups directory {}: {error}",
                state.runtime_config.settings.backups_dir.display()
            ))
        })?;

    let filename = format!("config-{}.json", Utc::now().format("%Y%m%d-%H%M%S-%3f"));
    let destination = state.runtime_config.settings.backups_dir.join(filename);

    fs::copy(source, &destination).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to create backup {}: {error}",
            destination.display()
        ))
    })?;

    backup_entry_from_path(&destination).await
}

async fn read_backups(state: &AppState) -> Result<Vec<BackupEntry>, ApiError> {
    let backups_dir = &state.runtime_config.settings.backups_dir;

    if !fs::try_exists(backups_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to check backups directory {}: {error}",
            backups_dir.display()
        ))
    })? {
        return Ok(Vec::new());
    }

    let mut entries = fs::read_dir(backups_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to read backups directory {}: {error}",
            backups_dir.display()
        ))
    })?;
    let mut backups = Vec::new();

    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        ApiError::internal(format!(
            "failed to iterate backups directory {}: {error}",
            backups_dir.display()
        ))
    })? {
        let file_type = entry.file_type().await.map_err(|error| {
            ApiError::internal(format!(
                "failed to inspect backup entry {}: {error}",
                entry.path().display()
            ))
        })?;
        if !file_type.is_file() {
            continue;
        }

        backups.push(backup_candidate_from_path(&entry.path()).await?);
    }

    backups.sort_by(|left, right| right.sort_key.cmp(&left.sort_key));
    Ok(backups.into_iter().map(|item| item.entry).collect())
}

async fn read_srs_files(state: &AppState) -> Result<Vec<SrsEntry>, ApiError> {
    let srs_dir = &state.runtime_config.settings.srs_dir;

    if !fs::try_exists(srs_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to check srs directory {}: {error}",
            srs_dir.display()
        ))
    })? {
        return Ok(Vec::new());
    }

    let mut entries = fs::read_dir(srs_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to read srs directory {}: {error}",
            srs_dir.display()
        ))
    })?;
    let mut files = Vec::new();

    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        ApiError::internal(format!(
            "failed to iterate srs directory {}: {error}",
            srs_dir.display()
        ))
    })? {
        let file_type = entry.file_type().await.map_err(|error| {
            ApiError::internal(format!(
                "failed to inspect srs entry {}: {error}",
                entry.path().display()
            ))
        })?;
        if !file_type.is_file() {
            continue;
        }

        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("srs") {
            continue;
        }

        files.push(srs_candidate_from_path(&entry.path()).await?);
    }

    files.sort_by(|left, right| right.sort_key.cmp(&left.sort_key));
    Ok(files.into_iter().map(|item| item.entry).collect())
}

async fn backup_entry_from_path(path: &StdPath) -> Result<BackupEntry, ApiError> {
    Ok(backup_candidate_from_path(path).await?.entry)
}

async fn backup_candidate_from_path(path: &StdPath) -> Result<BackupCandidate, ApiError> {
    let metadata = fs::metadata(path).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to read metadata for {}: {error}",
            path.display()
        ))
    })?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let modified_at = DateTime::<Utc>::from(modified).to_rfc3339();
    let sort_key = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ApiError::internal(format!("invalid backup filename: {}", path.display())))?
        .to_string();

    Ok(BackupCandidate {
        entry: BackupEntry {
            name,
            path: path.display().to_string(),
            size_bytes: metadata.len(),
            modified_at,
        },
        sort_key,
    })
}

async fn srs_candidate_from_path(path: &StdPath) -> Result<SrsCandidate, ApiError> {
    let metadata = fs::metadata(path).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to read metadata for {}: {error}",
            path.display()
        ))
    })?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let modified_at = DateTime::<Utc>::from(modified).to_rfc3339();
    let sort_key = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ApiError::internal(format!("invalid srs filename: {}", path.display())))?
        .to_string();

    Ok(SrsCandidate {
        entry: SrsEntry {
            name,
            path: path.display().to_string(),
            size_bytes: metadata.len(),
            modified_at,
        },
        sort_key,
    })
}

fn sanitize_backup_name(name: &str) -> Result<String, ApiError> {
    let parsed = StdPath::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ApiError::bad_request("backup name is invalid"))?;

    if parsed != name {
        return Err(ApiError::bad_request(
            "backup name must not contain path separators",
        ));
    }

    Ok(parsed.to_string())
}

fn sanitize_srs_name(name: &str) -> Result<String, ApiError> {
    let parsed = StdPath::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ApiError::bad_request("srs file name is invalid"))?;

    if parsed != name {
        return Err(ApiError::bad_request(
            "srs file name must not contain path separators",
        ));
    }

    if !parsed.ends_with(".srs") {
        return Err(ApiError::bad_request("only .srs files are allowed"));
    }

    Ok(parsed.to_string())
}

fn parse_srs_download_url(raw_url: &str) -> Result<(Url, String), ApiError> {
    let url = Url::parse(raw_url)
        .map_err(|error| ApiError::bad_request(format!("invalid srs url: {error}")))?;

    if url.scheme() != "https" {
        return Err(ApiError::bad_request("srs url must use https"));
    }

    let host = url
        .host_str()
        .ok_or_else(|| ApiError::bad_request("srs url host is missing"))?;
    if host != "github.com" && host != "raw.githubusercontent.com" {
        return Err(ApiError::bad_request(
            "only github.com and raw.githubusercontent.com are allowed",
        ));
    }

    let file_name = url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ApiError::bad_request("srs url must end with a file name"))?;
    let file_name = sanitize_srs_name(file_name)?;

    Ok((url, file_name))
}

#[cfg(test)]
mod tests {
    use super::{
        api_router, build_check_command_parts, parse_srs_download_url, sanitize_backup_name,
        sanitize_srs_name,
    };
    use crate::{
        AppState,
        config::{AppConfig, RuntimeConfig},
    };
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::fs;
    use tower::util::ServiceExt;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn rejects_backup_path_traversal() {
        assert!(sanitize_backup_name("../config.json").is_err());
    }

    #[test]
    fn rejects_non_srs_download_names() {
        assert!(sanitize_srs_name("ruleset.txt").is_err());
        assert!(sanitize_srs_name("../ruleset.srs").is_err());
    }

    #[test]
    fn accepts_github_srs_download_url() {
        let (_, file_name) = parse_srs_download_url(
            "https://github.com/KaringX/karing-ruleset/raw/refs/heads/sing/russia/antizapret/antizapret.srs",
        )
        .expect("download url");

        assert_eq!(file_name, "antizapret.srs");
    }

    #[test]
    fn rejects_non_github_srs_download_url() {
        assert!(parse_srs_download_url("https://example.com/file.srs").is_err());
        assert!(parse_srs_download_url("http://github.com/file.srs").is_err());
    }

    #[test]
    fn check_command_replaces_config_placeholder() {
        let parts = build_check_command_parts(
            "sudo -n /usr/local/libexec/sing-box-config-api/check {config_path}",
            Path::new("/tmp/check.json"),
        )
        .expect("command parts");

        assert_eq!(
            parts,
            vec![
                "sudo",
                "-n",
                "/usr/local/libexec/sing-box-config-api/check",
                "/tmp/check.json"
            ]
        );
    }

    #[test]
    fn check_command_appends_path_when_placeholder_missing() {
        let parts = build_check_command_parts(
            "sudo -n /usr/local/libexec/sing-box-config-api/check",
            Path::new("/tmp/check.json"),
        )
        .expect("command parts");

        assert_eq!(
            parts,
            vec![
                "sudo",
                "-n",
                "/usr/local/libexec/sing-box-config-api/check",
                "/tmp/check.json"
            ]
        );
    }

    #[tokio::test]
    async fn restore_route_accepts_dot_json_backup_names() {
        let fixture = TestFixture::new().await;
        let app = fixture.app();

        let request = Request::builder()
            .method("POST")
            .uri("/api/backups/config-20260415-185504-710.json/restore")
            .header("x-api-secret", fixture.secret())
            .body(Body::empty())
            .expect("request");

        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body_text = std::str::from_utf8(&body).expect("utf8");

        assert_eq!(status, StatusCode::OK, "{body_text}");
        assert!(body_text.contains("config-20260415-185504-710.json"));
    }

    #[tokio::test]
    async fn restore_missing_backup_returns_json_404() {
        let fixture = TestFixture::new().await;
        let app = fixture.app();

        let request = Request::builder()
            .method("POST")
            .uri("/api/backups/missing.json/restore")
            .header("x-api-secret", fixture.secret())
            .body(Body::empty())
            .expect("request");

        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        assert!(
            std::str::from_utf8(&body)
                .expect("utf8")
                .contains("backup not found")
        );
    }

    #[tokio::test]
    async fn list_srs_only_returns_srs_files() {
        let fixture = TestFixture::new().await;
        let app = fixture.app();

        let request = Request::builder()
            .method("GET")
            .uri("/api/srs")
            .header("x-api-secret", fixture.secret())
            .body(Body::empty())
            .expect("request");

        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body_text = std::str::from_utf8(&body).expect("utf8");

        assert_eq!(status, StatusCode::OK, "{body_text}");
        assert!(body_text.contains("ruleset.srs"));
        assert!(!body_text.contains("ignore.txt"));
    }

    struct TestFixture {
        root: PathBuf,
        state: AppState,
    }

    impl TestFixture {
        async fn new() -> Self {
            let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("sing-box-config-api-test-{suffix}-{fixture_id}"));
            let backups_dir = root.join("backups");
            let srs_dir = root.join("srs");
            let config_path = root.join("config.json");
            let backup_path = backups_dir.join("config-20260415-185504-710.json");

            fs::create_dir_all(&backups_dir).await.expect("backups dir");
            fs::create_dir_all(&srs_dir).await.expect("srs dir");
            fs::write(&config_path, "{\"before\":true}\n")
                .await
                .expect("config");
            fs::write(&backup_path, "{\"restored\":true}\n")
                .await
                .expect("backup");
            fs::write(srs_dir.join("ruleset.srs"), "srs-bytes")
                .await
                .expect("srs file");
            fs::write(srs_dir.join("ignore.txt"), "ignore")
                .await
                .expect("non-srs file");

            let settings = AppConfig {
                secret: "secret".to_string(),
                listen_addr: "127.0.0.1:17118".to_string(),
                sing_box_config_path: config_path,
                backups_dir,
                srs_dir,
                status_command: "true".to_string(),
                check_command: "true".to_string(),
                restart_command: "true".to_string(),
            };
            let state = AppState {
                runtime_config: Arc::new(RuntimeConfig {
                    settings,
                    config_source: None,
                }),
            };

            Self { root, state }
        }

        fn app(&self) -> Router {
            Router::new()
                .nest("/api", api_router(self.state.clone()))
                .with_state(self.state.clone())
        }

        fn secret(&self) -> &str {
            &self.state.runtime_config.settings.secret
        }
    }

    impl Drop for TestFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
