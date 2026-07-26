use std::{env, fs, path::PathBuf, process::Command};

use chrono::Local;
use toml_edit::{table, value, DocumentMut};

use serde::Deserialize;

use crate::{models::{AppSettings, CodexApplyResult, CodexRestartResult, CodexStatus}, RelayError};

/// `~/.codex`, where Codex keeps its config, rollout files and session index.
pub fn codex_home() -> Result<PathBuf, RelayError> {
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).ok_or_else(|| RelayError::InvalidInput("无法定位用户目录".into()))?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn config_path() -> Result<PathBuf, RelayError> {
    Ok(codex_home()?.join("config.toml"))
}

pub fn status(gateway_port: u16) -> CodexStatus {
    let Ok(path) = config_path() else { return CodexStatus::default(); };
    let mut status = CodexStatus { config_path: path.display().to_string(), ..CodexStatus::default() };
    let Ok(content) = fs::read_to_string(&path) else { return status; };
    let Ok(document) = content.parse::<DocumentMut>() else { return status; };
    status.active_provider = document.get("model_provider").and_then(|item| item.as_str()).map(str::to_owned);
    status.active_model = document.get("model").and_then(|item| item.as_str()).map(str::to_owned);
    let block = document.get("model_providers").and_then(|item| item.get("relaydeck"));
    status.configured_port = block
        .and_then(|item| item.get("base_url"))
        .and_then(|item| item.as_str())
        .and_then(configured_port);
    status.configured = status.active_provider.as_deref() == Some("relaydeck") && block.is_some();
    status.stale = status.configured && status.configured_port != Some(gateway_port);
    status
}

/// Reads the port out of a `http://127.0.0.1:PORT/v1` style base URL.
fn configured_port(base_url: &str) -> Option<u16> {
    base_url
        .rsplit_once(':')
        .map(|(_, tail)| tail.trim_start_matches('/'))
        .and_then(|tail| tail.split('/').next())
        .and_then(|port| port.parse().ok())
}

pub fn apply(settings: &AppSettings, model: &str, provider_name: &str) -> Result<CodexApplyResult, RelayError> {
    let path = config_path()?;
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut document = if content.trim().is_empty() { DocumentMut::new() } else { content.parse::<DocumentMut>().map_err(|error| RelayError::InvalidInput(format!("Codex config.toml 无法解析: {error}")))? };

    let backup_path = if path.exists() {
        let backup = path.with_file_name(format!("config.toml.relaydeck-backup-{}", Local::now().format("%Y%m%d-%H%M%S")));
        fs::copy(&path, &backup)?;
        prune_backups(&path, BACKUPS_TO_KEEP);
        Some(backup.display().to_string())
    } else { None };

    configure_document(&mut document, settings, model);
    fs::write(&path, document.to_string())?;
    write_codex_env(&path, &settings.local_access_key)?;
    set_user_environment_key(&settings.local_access_key)?;

    Ok(CodexApplyResult { config_path: path.display().to_string(), backup_path, model: model.into(), provider_name: provider_name.into(), aligned_sessions: 0 })
}

const BACKUP_PREFIX: &str = "config.toml.relaydeck-backup-";
const BACKUPS_TO_KEEP: usize = 5;

/// Keeps the newest backups so repeated applies do not litter `~/.codex`.
fn prune_backups(config_path: &PathBuf, keep: usize) {
    let Some(parent) = config_path.parent() else { return };
    let Ok(entries) = fs::read_dir(parent) else { return };
    let mut backups = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(BACKUP_PREFIX)))
        .collect::<Vec<_>>();
    if backups.len() <= keep {
        return;
    }
    backups.sort();
    for stale in backups.iter().take(backups.len() - keep) {
        let _ = fs::remove_file(stale);
    }
}

fn write_codex_env(config_path: &PathBuf, key: &str) -> Result<(), RelayError> {
    let env_path = config_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join(".env");
    let content = fs::read_to_string(&env_path).unwrap_or_default();
    let mut found = false;
    let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
    for line in &mut lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("RELAYDECK_API_KEY=") || trimmed.starts_with("export RELAYDECK_API_KEY=") {
            *line = format!("RELAYDECK_API_KEY={key}");
            found = true;
        }
    }
    if !found { lines.push(format!("RELAYDECK_API_KEY={key}")); }
    fs::write(env_path, format!("{}\n", lines.join("\n")))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestartTarget {
    kind: String,
    launch: String,
    app_name: String,
    version: Option<String>,
    installed_at: Option<String>,
}

#[cfg(windows)]
pub(crate) fn discover_restart_target() -> Result<RestartTarget, RelayError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const SCRIPT: &str = r#"
$targets = @()
Get-AppxPackage -Name 'OpenAI.Codex' -ErrorAction SilentlyContinue | ForEach-Object {
  $pkg = $_
  $manifest = Get-AppxPackageManifest -Package $pkg.PackageFullName
  $application = $manifest.Package.Applications.Application | Where-Object { $_.Executable -match 'ChatGPT' } | Select-Object -First 1
  if (-not $application) { $application = $manifest.Package.Applications.Application | Select-Object -First 1 }
  if ($application) {
    $install = Get-Item -LiteralPath $pkg.InstallLocation -ErrorAction SilentlyContinue
    $stamp = if ($install) { $install.LastWriteTimeUtc } else { [datetime]::MinValue }
    $targets += [pscustomobject]@{
      kind = 'appx'; launch = $pkg.PackageFamilyName + '!' + $application.Id; appName = 'ChatGPT'
      version = [string]$pkg.Version; installedAt = $stamp.ToString('o'); sort = $stamp.Ticks
    }
  }
}
$localRoots = @("$env:LOCALAPPDATA\Programs\ChatGPT", "$env:LOCALAPPDATA\OpenAI\ChatGPT")
foreach ($root in $localRoots) {
  Get-ChildItem -LiteralPath $root -Filter 'ChatGPT.exe' -File -Recurse -ErrorAction SilentlyContinue | ForEach-Object {
    $targets += [pscustomobject]@{
      kind = 'exe'; launch = $_.FullName; appName = 'ChatGPT'
      version = $_.VersionInfo.ProductVersion; installedAt = $_.LastWriteTimeUtc.ToString('o'); sort = $_.LastWriteTimeUtc.Ticks
    }
  }
}
if (-not $targets) {
  Get-Process -Name 'ChatGPT' -ErrorAction SilentlyContinue | Where-Object { $_.Path } | ForEach-Object {
    $path = $_.Path
    if ($path -match '\\WindowsApps\\(?<package>OpenAI\.Codex_[^\\]+)\\') {
      $package = $Matches.package
      if ($package -match '^(?<name>.+?)_[^_]+_[^_]+__(?<publisher>.+)$') {
        $folder = Get-Item -LiteralPath (Split-Path (Split-Path $path -Parent) -Parent) -ErrorAction SilentlyContinue
        $stamp = if ($folder) { $folder.LastWriteTimeUtc } else { $_.StartTime.ToUniversalTime() }
        $targets += [pscustomobject]@{
          kind = 'appx'; launch = $Matches.name + '_' + $Matches.publisher + '!App'; appName = 'ChatGPT'
          version = $null; installedAt = $stamp.ToString('o'); sort = $stamp.Ticks
        }
      }
    } else {
      $file = Get-Item -LiteralPath $path -ErrorAction SilentlyContinue
      if ($file) {
        $targets += [pscustomobject]@{
          kind = 'exe'; launch = $file.FullName; appName = 'ChatGPT'
          version = $file.VersionInfo.ProductVersion; installedAt = $file.LastWriteTimeUtc.ToString('o'); sort = $file.LastWriteTimeUtc.Ticks
        }
      }
    }
  }
}
if (-not $targets) {
  Get-StartApps | Where-Object { $_.Name -match 'ChatGPT|Codex' } | ForEach-Object {
    $targets += [pscustomobject]@{ kind = 'appx'; launch = $_.AppID; appName = $_.Name; version = $null; installedAt = $null; sort = 0 }
  }
}
$target = $targets | Sort-Object -Property @{ Expression = 'sort'; Descending = $true } | Select-Object -First 1
if ($target) { $target | Select-Object kind,launch,appName,version,installedAt | ConvertTo-Json -Compress }
"#;
    let discover = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    let payload = String::from_utf8_lossy(&discover.stdout);
    if payload.trim().is_empty() { return Err(RelayError::InvalidInput("没有找到已安装的 ChatGPT/Codex 桌面应用".into())); }
    serde_json::from_str(payload.trim()).map_err(|error| RelayError::InvalidInput(format!("无法解析 ChatGPT/Codex 安装信息: {error}")))
}

#[cfg(windows)]
pub(crate) fn stop_running() -> Result<(), RelayError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "Get-Process -Name 'ChatGPT','Codex','codex-code-mode-host','codex-command-runner-*' -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 800"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if output.status.success() { Ok(()) } else { Err(RelayError::InvalidInput("无法关闭正在运行的 ChatGPT/Codex".into())) }
}

#[cfg(windows)]
pub(crate) fn launch(target: RestartTarget) -> Result<CodexRestartResult, RelayError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let launch = target.launch.replace('\'', "''");
    let script = if target.kind == "exe" {
        format!("Start-Process -FilePath '{launch}'")
    } else {
        format!("Start-Process explorer.exe -ArgumentList 'shell:AppsFolder\\{launch}'")
    };
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() { return Err(RelayError::InvalidInput("无法启动最新的 ChatGPT/Codex 桌面应用".into())); }
    Ok(CodexRestartResult { app_name: target.app_name, version: target.version, installed_at: target.installed_at })
}

#[cfg(windows)]
pub fn restart() -> Result<CodexRestartResult, RelayError> {
    let target = discover_restart_target()?;
    stop_running()?;
    launch(target)
}

#[cfg(not(windows))]
pub fn restart() -> Result<CodexRestartResult, RelayError> {
    Err(RelayError::InvalidInput("Codex 自动重启目前仅支持 Windows".into()))
}

/// Selects a file in the system file manager so the user can inspect or copy it.
#[cfg(windows)]
pub fn reveal(path: &str) -> Result<(), RelayError> {
    Command::new("explorer.exe").arg(format!("/select,{path}")).spawn()?;
    Ok(())
}

#[cfg(not(windows))]
pub fn reveal(_path: &str) -> Result<(), RelayError> {
    Err(RelayError::InvalidInput("打开文件夹目前仅支持 Windows".into()))
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
    document["model_providers"]["relaydeck"]["requires_openai_auth"] = value(false);
    document["model_providers"]["relaydeck"]["supports_websockets"] = value(false);
    // RelayDeck already fails over between upstreams; these let Codex ride out a switch
    // instead of surfacing a transport error to the user mid-turn.
    document["model_providers"]["relaydeck"]["request_max_retries"] = value(2);
    document["model_providers"]["relaydeck"]["stream_max_retries"] = value(3);
    let idle_ms = settings.stream_idle_timeout_seconds.clamp(10, 3600) as i64 * 1000;
    document["model_providers"]["relaydeck"]["stream_idle_timeout_ms"] = value(idle_ms);
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
    use super::{configure_document, configured_port, prune_backups, write_codex_env, BACKUP_PREFIX};
    use crate::models::AppSettings;
    use std::{fs, path::PathBuf};
    use toml_edit::DocumentMut;

    #[test]
    fn preserves_unrelated_codex_configuration() {
        let mut document = "sandbox_mode = \"workspace-write\"\n[mcp_servers.docs]\nurl = \"https://example.test\"\n".parse::<DocumentMut>().unwrap();
        let settings = AppSettings { gateway_port: 1455, stream_idle_timeout_seconds: 180, ..AppSettings::default() };
        configure_document(&mut document, &settings, "gpt-5.6-sol");
        assert_eq!(document["sandbox_mode"].as_str(), Some("workspace-write"));
        assert_eq!(document["mcp_servers"]["docs"]["url"].as_str(), Some("https://example.test"));
        assert_eq!(document["model_provider"].as_str(), Some("relaydeck"));
        assert_eq!(document["model_providers"]["relaydeck"]["base_url"].as_str(), Some("http://127.0.0.1:1455/v1"));
        assert_eq!(document["model_providers"]["relaydeck"]["requires_openai_auth"].as_bool(), Some(false));
        assert_eq!(document["model_providers"]["relaydeck"]["stream_idle_timeout_ms"].as_integer(), Some(180_000));
        assert_eq!(document["model_providers"]["relaydeck"]["stream_max_retries"].as_integer(), Some(3));
    }

    #[test]
    fn detects_the_port_codex_currently_points_at() {
        assert_eq!(configured_port("http://127.0.0.1:1455/v1"), Some(1455));
        assert_eq!(configured_port("http://127.0.0.1:8080"), Some(8080));
        assert_eq!(configured_port("https://relay.example/v1"), None);
    }

    #[test]
    fn keeps_only_the_newest_backups() {
        let root = std::env::temp_dir().join(format!("relaydeck-backup-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config.toml");
        fs::write(&config, "model = \"gpt-5.6-sol\"\n").unwrap();
        for index in 0..8 {
            fs::write(root.join(format!("{BACKUP_PREFIX}2026010{index}-000000")), "old").unwrap();
        }
        prune_backups(&config, 3);
        let mut remaining = fs::read_dir(&root).unwrap().flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(BACKUP_PREFIX))
            .collect::<Vec<_>>();
        remaining.sort();
        assert_eq!(remaining, vec![
            format!("{BACKUP_PREFIX}20260105-000000"),
            format!("{BACKUP_PREFIX}20260106-000000"),
            format!("{BACKUP_PREFIX}20260107-000000"),
        ]);
        assert!(config.exists());
        let _ = fs::remove_dir_all(root);
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
