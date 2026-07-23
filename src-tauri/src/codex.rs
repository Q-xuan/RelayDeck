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
    write_codex_env(&path, &settings.local_access_key)?;
    set_user_environment_key(&settings.local_access_key)?;

    Ok(CodexApplyResult { config_path: path.display().to_string(), backup_path, model: model.into() })
}

fn write_codex_env(config_path: &PathBuf, key: &str) -> Result<(), RelayError> {
    let env_path = config_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join(".env");
    let content = fs::read_to_string(&env_path).unwrap_or_default();
    let mut found = false;
    let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
    for line in &mut lines {
        if line.trim_start().starts_with("RELAYDECK_API_KEY=") {
            *line = format!("RELAYDECK_API_KEY={key}");
            found = true;
        }
    }
    if !found { lines.push(format!("RELAYDECK_API_KEY={key}")); }
    fs::write(env_path, format!("{}\n", lines.join("\n")))?;
    Ok(())
}

#[cfg(windows)]
pub fn restart() -> Result<(), RelayError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let discover = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "$app = Get-StartApps | Where-Object { $_.Name -match 'Codex' } | Select-Object -First 1; if ($app) { $app.AppID } else { $pkg = Get-AppxPackage -Name 'OpenAI.Codex' | Select-Object -First 1; if ($pkg) { $manifest = Get-AppxPackageManifest -Package $pkg.PackageFullName; $application = $manifest.Package.Applications.Application | Select-Object -First 1; $pkg.PackageFamilyName + '!' + $application.Id } }"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    let app_id = String::from_utf8_lossy(&discover.stdout).trim().replace('\'', "''");
    if app_id.is_empty() { return Err(RelayError::InvalidInput("没有找到已安装的 Codex 桌面应用".into())); }
    let script = format!("Start-Sleep -Milliseconds 700; Get-Process -Name 'Codex' -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Seconds 2; Start-Process 'shell:AppsFolder\\{app_id}'");
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
pub fn restart() -> Result<(), RelayError> {
    Err(RelayError::InvalidInput("Codex 自动重启目前仅支持 Windows".into()))
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
    use super::{configure_document, write_codex_env};
    use crate::models::AppSettings;
    use std::{fs, path::PathBuf};
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

    #[test]
    fn updates_codex_env_without_dropping_other_values() {
        let root = std::env::temp_dir().join(format!("relaydeck-codex-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let config = PathBuf::from(root.join("config.toml"));
        fs::write(root.join(".env"), "OTHER_VALUE=keep\nRELAYDECK_API_KEY=old\n").unwrap();
        write_codex_env(&config, "rd_local_new").unwrap();
        let content = fs::read_to_string(root.join(".env")).unwrap();
        assert!(content.contains("OTHER_VALUE=keep"));
        assert!(content.contains("RELAYDECK_API_KEY=rd_local_new"));
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(not(windows))]
fn set_user_environment_key(key: &str) -> Result<(), RelayError> {
    env::set_var("RELAYDECK_API_KEY", key);
    Ok(())
}
