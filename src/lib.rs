use std::{env, fs, path::PathBuf};

use serde::Deserialize;
use zed_extension_api as zed;
use zed_extension_api::{
    process::Command,
    serde_json::{self, json},
    settings::{CommandSettings, LspSettings},
    LanguageServerId, LanguageServerInstallationStatus, Result,
};

const EXTENSION_ID: &str = "shanhai-translate";
const SERVER_ID: &str = "shanhai-translate-lsp";
const SERVER_BINARY_NAME: &str = "shanhai-translate-lsp-server";
const DEFAULT_API_BASE_URL: &str = "https://apione.apibyte.cn/translate";
const DEFAULT_API_KEY: &str = "";

struct ShanHaiTranslateExtension {
    cached_binary_path: Option<String>,
}

#[derive(Default, Clone, Deserialize)]
struct ExtensionRuntimeSettings {
    #[serde(alias = "githubRepo", alias = "serverGithubRepo")]
    github_repo: Option<String>,
    #[serde(alias = "githubReleaseTag", alias = "serverGithubReleaseTag")]
    github_release_tag: Option<String>,
}

impl ShanHaiTranslateExtension {
    fn installed_extension_dir() -> Result<PathBuf> {
        let work_dir = env::current_dir()
            .map_err(|error| format!("failed to read extension work directory: {error}"))?;
        let extensions_dir = work_dir
            .parent()
            .and_then(|path| path.parent())
            .ok_or_else(|| {
                format!(
                    "unexpected extension work directory: {}",
                    work_dir.display()
                )
            })?;

        Ok(extensions_dir.join("installed").join(EXTENSION_ID))
    }

    fn platform_target_dir() -> Result<&'static str> {
        let (os, arch) = zed::current_platform();
        match (os, arch) {
            (zed::Os::Mac, zed::Architecture::Aarch64) => Ok("darwin-aarch64"),
            (zed::Os::Mac, zed::Architecture::X8664) => Ok("darwin-x86_64"),
            (zed::Os::Linux, zed::Architecture::Aarch64) => Ok("linux-aarch64"),
            (zed::Os::Linux, zed::Architecture::X8664) => Ok("linux-x86_64"),
            (zed::Os::Windows, zed::Architecture::X8664) => Ok("windows-x86_64"),
            _ => Err(format!(
                "unsupported platform for {}: {:?} {:?}",
                SERVER_BINARY_NAME, os, arch
            )),
        }
    }

    fn executable_name() -> String {
        match zed::current_platform().0 {
            zed::Os::Windows => format!("{SERVER_BINARY_NAME}.exe"),
            _ => SERVER_BINARY_NAME.to_string(),
        }
    }

    fn bundled_binary_path() -> Result<Option<String>> {
        let executable_name = Self::executable_name();
        let target_dir = Self::platform_target_dir()?;
        let mut candidates = vec![PathBuf::from("bin")
            .join(target_dir)
            .join(executable_name.clone())];

        if let Ok(installed_extension_dir) = Self::installed_extension_dir() {
            candidates.push(
                installed_extension_dir
                    .join("bin")
                    .join(target_dir)
                    .join(executable_name),
            );
        }

        for candidate in candidates {
            if fs::metadata(&candidate).is_ok_and(|stat| stat.is_file()) {
                return Ok(Some(candidate.to_string_lossy().into_owned()));
            }
        }

        Ok(None)
    }

    fn merged_settings(lsp_settings: &LspSettings) -> serde_json::Value {
        let mut settings = json!({
            "api_base_url": DEFAULT_API_BASE_URL,
            "api_key": DEFAULT_API_KEY,
            "timeout_ms": 15000,
            "debounce_ms": 350,
            "error_cache_ttl_ms": 2000
        });

        if let Some(user_settings) = lsp_settings.settings.clone() {
            merge_json(&mut settings, user_settings);
        }

        if let Some(initialization_options) = lsp_settings.initialization_options.clone() {
            merge_json(&mut settings, initialization_options);
        }

        settings
    }

    fn server_settings(worktree: &zed::Worktree) -> Result<serde_json::Value> {
        let lsp_settings = LspSettings::for_worktree(SERVER_ID, worktree).unwrap_or_default();
        Ok(Self::merged_settings(&lsp_settings))
    }

    fn runtime_settings(lsp_settings: &LspSettings) -> ExtensionRuntimeSettings {
        serde_json::from_value(Self::merged_settings(lsp_settings)).unwrap_or_default()
    }

    fn language_server_command_from_settings(
        path: String,
        command_settings: Option<&CommandSettings>,
    ) -> Command {
        let mut command = Command::new(path);

        if let Some(arguments) = command_settings.and_then(|settings| settings.arguments.clone()) {
            command = command.args(arguments);
        }

        if let Some(env) = command_settings.and_then(|settings| settings.env.clone()) {
            command = command.envs(env);
        }

        command
    }

    fn github_repo(runtime_settings: &ExtensionRuntimeSettings) -> Option<String> {
        runtime_settings
            .github_repo
            .as_deref()
            .and_then(parse_github_repo)
            .or_else(|| {
                parse_github_repo_from_extension_manifest(include_str!("../extension.toml"))
            })
    }

    fn zed_managed_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        runtime_settings: &ExtensionRuntimeSettings,
    ) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        let github_repo = Self::github_repo(runtime_settings).ok_or_else(|| {
            format!(
                "no GitHub repository configured for {}. Update extension.toml's repository field to a GitHub repo or set lsp.{}.settings.github_repo",
                EXTENSION_ID, SERVER_ID
            )
        })?;

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = if let Some(tag) = runtime_settings
            .github_release_tag
            .as_deref()
            .and_then(normalize_nonempty)
        {
            zed::github_release_by_tag_name(&github_repo, tag)
                .map_err(|error| self.fail_install(language_server_id, error))?
        } else {
            zed::latest_github_release(
                &github_repo,
                zed::GithubReleaseOptions {
                    require_assets: true,
                    pre_release: false,
                },
            )
            .map_err(|error| self.fail_install(language_server_id, error))?
        };

        let (asset_name, file_type, binary_path) = Self::release_asset_details(&release.version)?;
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| {
                self.fail_install(
                    language_server_id,
                    format!("no release asset found matching {asset_name}"),
                )
            })?;

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );

            let version_dir = release_directory_name(&release.version);
            zed::download_file(&asset.download_url, &version_dir, file_type)
                .map_err(|error| self.fail_install(language_server_id, error))?;

            if !matches!(zed::current_platform().0, zed::Os::Windows) {
                zed::make_file_executable(&binary_path)
                    .map_err(|error| self.fail_install(language_server_id, error))?;
            }

            Self::prune_old_release_directories(&version_dir);
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::None,
        );

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    fn fail_install(
        &self,
        language_server_id: &LanguageServerId,
        message: impl Into<String>,
    ) -> String {
        let message = message.into();
        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::Failed(message.clone()),
        );
        message
    }

    fn prune_old_release_directories(current_version_dir: &str) {
        if let Ok(entries) = fs::read_dir(".") {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };

                if file_name == current_version_dir || !file_name.starts_with(SERVER_BINARY_NAME) {
                    continue;
                }

                if path.is_dir() {
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
    }

    fn release_asset_details(version: &str) -> Result<(String, zed::DownloadedFileType, String)> {
        let target_dir = Self::platform_target_dir()?;
        let executable_name = Self::executable_name();
        let version_dir = release_directory_name(version);

        match zed::current_platform().0 {
            zed::Os::Windows => Ok((
                format!("{SERVER_BINARY_NAME}-{target_dir}.zip"),
                zed::DownloadedFileType::Zip,
                format!("{version_dir}/{executable_name}"),
            )),
            _ => Ok((
                format!("{SERVER_BINARY_NAME}-{target_dir}.tar.gz"),
                zed::DownloadedFileType::GzipTar,
                format!("{version_dir}/{executable_name}"),
            )),
        }
    }
}

impl zed::Extension for ShanHaiTranslateExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        if language_server_id.as_ref() != SERVER_ID {
            return Err(format!(
                "unsupported language server requested: {}",
                language_server_id.as_ref()
            ));
        }

        let lsp_settings = LspSettings::for_worktree(SERVER_ID, worktree).unwrap_or_default();
        let command_settings = lsp_settings.binary.as_ref();

        if let Some(path) = command_settings
            .and_then(|settings| settings.path.clone())
            .and_then(|path| normalize_nonempty(&path).map(ToString::to_string))
        {
            return Ok(Self::language_server_command_from_settings(
                path,
                command_settings,
            ));
        }

        if let Some(path) = Self::bundled_binary_path()? {
            return Ok(Self::language_server_command_from_settings(
                path,
                command_settings,
            ));
        }

        let runtime_settings = Self::runtime_settings(&lsp_settings);
        let path = self.zed_managed_binary_path(language_server_id, &runtime_settings)?;
        Ok(Self::language_server_command_from_settings(
            path,
            command_settings,
        ))
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        if language_server_id.as_ref() != SERVER_ID {
            return Ok(None);
        }

        Ok(Some(Self::server_settings(worktree)?))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        if language_server_id.as_ref() != SERVER_ID {
            return Ok(None);
        }

        Ok(Some(Self::server_settings(worktree)?))
    }
}

zed::register_extension!(ShanHaiTranslateExtension);

fn release_directory_name(version: &str) -> String {
    format!("{SERVER_BINARY_NAME}-{version}")
}

fn normalize_nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn parse_github_repo_from_extension_manifest(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let line = line.trim();
        let repository = line.strip_prefix("repository = ")?;
        let repository = repository.trim().trim_matches('"');
        parse_github_repo(repository)
    })
}

fn parse_github_repo(value: &str) -> Option<String> {
    let trimmed = normalize_nonempty(value)?;
    let trimmed = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .trim_end_matches(".git");

    let mut parts = trimmed.split('/');
    let owner = normalize_nonempty(parts.next()?)?;
    let repo = normalize_nonempty(parts.next()?)?;

    if parts.next().is_some() {
        return None;
    }

    Some(format!("{owner}/{repo}"))
}

fn merge_json(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target_map), serde_json::Value::Object(patch_map)) => {
            for (key, value) in patch_map {
                if let Some(existing) = target_map.get_mut(&key) {
                    merge_json(existing, value);
                } else {
                    target_map.insert(key, value);
                }
            }
        }
        (target_slot, patch_value) => {
            *target_slot = patch_value;
        }
    }
}
