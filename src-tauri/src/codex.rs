use std::{env, fs, path::PathBuf, process::Command};

use chrono::Local;
use toml_edit::{table, value, DocumentMut};

use crate::{models::{AppSettings, CodexApplyResult, CodexStatus}, RelayError};

fn config_path() -> Result<PathBuf, RelayError> {
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).ok_or_else(|| RelayError::InvalidInput("无法定位用户目录".into()))?;
    Ok(PathBuf::from(home).join(".codex").join("config.toml"))
}

pub fn status() -> CodexStatus {
    let Ok(path) = config_path() else { return CodexStatus::default(); };
    let mut status = CodexStatus { config_path: path.display().to_string(), ..CodexStatus::default() };
    let Ok(content) = fs::read_to_string(&path) else { return status; };
    let Ok(document) = content.parse::<DocumentMut>() else { return status; };
    status.active_provider = document.get("model_provider").and_then(|item| item.as_str()).map(str::to_owned);
    status.active_model = document.get("model").and_then(|item| item.as_str()).map(str::to_owned);
    status.configured = status.active_provider.as_deref() == Some("relaydeck") && document.get("model_providers")
        .and_then(|item| item.get("relaydeck")).is_some();
    status
}

pub fn apply(settings: &AppSettings, model: &str) -> Result<CodexApplyResult, RelayError> {
    let path = config_path()?;
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut document = if content.trim().is_empty() { DocumentMut::new() } else { content.parse::<DocumentMut>().map_err(|error| RelayError::InvalidInput(format!("Codex config.toml 无法解析: {error}")))? };

    let backup_path = if path.exists() {
        let backup = path.with_file_name(format!("config.toml.relaydeck-backup-{}", Local::now().format("%Y%m%d-%H%M%S")));
        fs::copy(&path, &backup)?;
        Some(backup.display().to_string())
    } else { None };

    configure_document(&mut document, settings, model);
    fs::write(&path, document.to_string())?;
    set_user_environment_key(&settings.local_access_key)?;

    Ok(CodexApplyResult { config_path: path.display().to_string(), backup_path, model: model.into() })
}

fn configure_document(document: &mut DocumentMut, settings: &AppSettings, model: &str) {
    document["model"] = value(model);
    document["model_provider"] = value("relaydeck");
    if !document.contains_key("model_providers") { document["model_providers"] = table(); }
    if document["model_providers"].get("relaydeck").is_none() { document["model_providers"]["relaydeck"] = table(); }
    document["model_providers"]["relaydeck"]["name"] = value("RelayDeck");
    document["model_providers"]["relaydeck"]["base_url"] = value(format!("http://127.0.0.1:{}/v1", settings.gateway_port));
    document["model_providers"]["relaydeck"]["env_key"] = value("RELAYDECK_API_KEY");
    document["model_providers"]["relaydeck"]["wire_api"] = value("responses");
}

#[cfg(windows)]
fn set_user_environment_key(key: &str) -> Result<(), RelayError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let output = Command::new("setx").args(["RELAYDECK_API_KEY", key]).creation_flags(CREATE_NO_WINDOW).output()?;
    if output.status.success() { Ok(()) } else { Err(RelayError::InvalidInput("无法写入用户环境变量 RELAYDECK_API_KEY".into())) }
}

#[cfg(test)]
mod tests {
    use super::configure_document;
    use crate::models::AppSettings;
    use toml_edit::DocumentMut;

    #[test]
    fn preserves_unrelated_codex_configuration() {
        let mut document = "sandbox_mode = \"workspace-write\"\n[mcp_servers.docs]\nurl = \"https://example.test\"\n".parse::<DocumentMut>().unwrap();
        let settings = AppSettings { gateway_port: 1455, ..AppSettings::default() };
        configure_document(&mut document, &settings, "gpt-5.6-sol");
        assert_eq!(document["sandbox_mode"].as_str(), Some("workspace-write"));
        assert_eq!(document["mcp_servers"]["docs"]["url"].as_str(), Some("https://example.test"));
        assert_eq!(document["model_provider"].as_str(), Some("relaydeck"));
        assert_eq!(document["model_providers"]["relaydeck"]["base_url"].as_str(), Some("http://127.0.0.1:1455/v1"));
    }
}

#[cfg(not(windows))]
fn set_user_environment_key(key: &str) -> Result<(), RelayError> {
    env::set_var("RELAYDECK_API_KEY", key);
    Ok(())
}
