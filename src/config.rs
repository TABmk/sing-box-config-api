use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env,
    path::{Path, PathBuf},
};

const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_SECRET: &str = "changeme";
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:17118";
const DEFAULT_SING_BOX_CONFIG_PATH: &str = "/etc/sing-box/config.json";
const DEFAULT_BACKUPS_DIR: &str = "/etc/sing-box/backups";
const DEFAULT_SRS_DIR: &str = "/etc/sing-box/srs";
const DEFAULT_STATUS_COMMAND: &str = "systemctl status sing-box --no-pager";
const DEFAULT_CHECK_COMMAND: &str = "sing-box check -c {config_path}";
const DEFAULT_RESTART_COMMAND: &str = "systemctl restart sing-box";

#[derive(Clone)]
pub struct RuntimeConfig {
    pub settings: AppConfig,
    pub config_source: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppConfig {
    pub secret: String,
    pub listen_addr: String,
    pub sing_box_config_path: PathBuf,
    pub backups_dir: PathBuf,
    pub srs_dir: PathBuf,
    pub status_command: String,
    pub check_command: String,
    pub restart_command: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawAppConfig {
    secret: Option<String>,
    listen_addr: Option<String>,
    sing_box_config_path: Option<PathBuf>,
    backups_dir: Option<PathBuf>,
    srs_dir: Option<PathBuf>,
    status_command: Option<String>,
    check_command: Option<String>,
    restart_command: Option<String>,
}

impl RawAppConfig {
    fn finalize(self, config_dir: Option<&Path>) -> AppConfig {
        let sing_box_config_path = resolve_path(
            self.sing_box_config_path
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SING_BOX_CONFIG_PATH)),
            config_dir,
        );
        let backups_dir = resolve_path(
            self.backups_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_BACKUPS_DIR)),
            config_dir,
        );
        let srs_dir = resolve_path(
            self.srs_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SRS_DIR)),
            config_dir,
        );

        AppConfig {
            secret: self.secret.unwrap_or_else(|| DEFAULT_SECRET.to_string()),
            listen_addr: self
                .listen_addr
                .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string()),
            status_command: self
                .status_command
                .unwrap_or_else(|| DEFAULT_STATUS_COMMAND.to_string()),
            check_command: self
                .check_command
                .unwrap_or_else(|| DEFAULT_CHECK_COMMAND.to_string()),
            restart_command: self
                .restart_command
                .unwrap_or_else(|| DEFAULT_RESTART_COMMAND.to_string()),
            sing_box_config_path,
            backups_dir,
            srs_dir,
        }
    }
}

pub fn load_runtime_config() -> Result<RuntimeConfig> {
    let config_source = locate_config_file()?;

    let settings = if let Some(path) = config_source.as_ref() {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let partial = toml::from_str::<RawAppConfig>(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        partial.finalize(path.parent())
    } else {
        RawAppConfig::default().finalize(None)
    };

    Ok(RuntimeConfig {
        settings,
        config_source,
    })
}

pub fn ensure_secure_secret(runtime_config: &RuntimeConfig) -> Result<()> {
    let secret = runtime_config.settings.secret.trim();
    if secret.is_empty() || secret == DEFAULT_SECRET {
        let source = describe_config_source(runtime_config.config_source.as_deref());
        bail!(
            "refusing to start with insecure secret. Set a strong 'secret' in {}",
            source
        );
    }

    Ok(())
}

pub fn describe_config_source(source: Option<&Path>) -> String {
    source
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("defaults (create {CONFIG_FILE_NAME} next to the binary)"))
}

fn locate_config_file() -> Result<Option<PathBuf>> {
    let exe_dir = env::current_exe()
        .context("failed to determine current executable path")?
        .parent()
        .map(Path::to_path_buf)
        .context("failed to determine executable directory")?;
    let cwd = env::current_dir().context("failed to determine current directory")?;

    let mut candidates = vec![exe_dir.join(CONFIG_FILE_NAME)];
    let cwd_candidate = cwd.join(CONFIG_FILE_NAME);
    if cwd_candidate != candidates[0] {
        candidates.push(cwd_candidate);
    }

    for candidate in candidates {
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

fn resolve_path(path: PathBuf, config_dir: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path
    } else if let Some(base) = config_dir {
        base.join(path)
    } else {
        path
    }
}
