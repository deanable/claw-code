#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::assigning_clones,
    clippy::cast_precision_loss,
    clippy::format_push_string,
    clippy::ignored_unit_patterns,
    clippy::if_same_then_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unit_arg,
    clippy::unused_self
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, IconData, RichText, TextEdit};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

const VC_REDIST_URL: &str =
    "https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170";
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const SYSTEM_PROMPT_ESTIMATE: u32 = 2_500;
const BASE_REQUEST_OVERHEAD: u32 = 1_500;
const REGISTRY_PATH: &str = "Software\\ClawLauncher";
const REGISTRY_STATE_VALUE: &str = "LauncherState";
const LAUNCH_PROMPT_ENV_VAR: &str = "CLAW_LAUNCH_PROMPT";
const LEGACY_DEFAULT_MODEL: &str = "llama-3.3-70b-versatile";
const NEW_PROVIDER_KEY: &str = "__new_provider__";
const LAUNCH_PROFILE_FILE_NAME: &str = ".claw-launch.json";
const LAUNCHER_LOG_FILE_NAME: &str = "claw-launcher.log";
const SANDBOX_SETTINGS_LOCAL_FILE_NAME: &str = "settings.local.json";
const LLAMA_CPP_BIN_DIR_NAME: &str = "llama-b8857-bin-win-cpu-x64";
const LLAMA_CPP_MODEL_PREFIX: &str = "llama.cpp/";
const LLAMA_CPP_SERVICE_NAME: &str = "ClawLlamaCpp";
const LLAMA_CPP_SERVICE_WRAPPER_EXE: &str = "claw-llama-service.exe";
const LLAMA_CPP_SERVICE_WRAPPER_EXE_FALLBACK: &str = "claw-llama-service2.exe";
const LLAMA_CPP_SERVICE_CONFIG_FILENAME: &str = "claw-llama-service.json";

fn default_sandbox_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ProviderKind {
    Groq,
    OpenRouter,
    GoogleAiStudio,
    Ollama,
    LlamaCpp,
    Custom,
}

impl ProviderKind {
    fn all() -> [Self; 6] {
        [
            Self::Groq,
            Self::OpenRouter,
            Self::GoogleAiStudio,
            Self::Ollama,
            Self::LlamaCpp,
            Self::Custom,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq",
            Self::OpenRouter => "OpenRouter",
            Self::GoogleAiStudio => "Google AI Studio",
            Self::Ollama => "Ollama (Local)",
            Self::LlamaCpp => "llama.cpp (Local)",
            Self::Custom => "Custom",
        }
    }

    fn supports_remote_models(self) -> bool {
        !matches!(self, Self::LlamaCpp)
    }

    fn requires_api_key(self) -> bool {
        !matches!(self, Self::Ollama | Self::LlamaCpp)
    }

    fn api_key_url(self) -> &'static str {
        match self {
            Self::Groq => "https://console.groq.com/keys",
            Self::OpenRouter => "https://openrouter.ai/keys",
            Self::GoogleAiStudio => "https://aistudio.google.com/app/apikey",
            Self::Ollama => "https://ollama.com/download",
            Self::LlamaCpp => "https://github.com/ggerganov/llama.cpp",
            Self::Custom => "https://platform.openai.com/api-keys",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderProfile {
    friendly_name: String,
    provider_kind: ProviderKind,
    api_key: String,
    #[serde(default)]
    huggingface_token: String,
    #[serde(default)]
    llama_cpp_server_path: PathBuf,
    base_url: String,
    workspace: PathBuf,
    model: String,
    permission_mode: String,
    allowed_tools: Vec<String>,
    keep_open: bool,
    prompt: String,
    args: Vec<String>,
    respect_rate_limits: bool,
    #[serde(default = "default_sandbox_enabled")]
    sandbox_enabled: bool,
    #[serde(default)]
    compact_output: bool,
    #[serde(default)]
    dangerously_skip_permissions: bool,
}

impl ProviderProfile {
    fn from_preset(preset: ProviderPreset) -> Self {
        Self {
            friendly_name: preset.name.to_string(),
            provider_kind: preset.kind,
            api_key: String::new(),
            huggingface_token: String::new(),
            llama_cpp_server_path: PathBuf::new(),
            base_url: preset.base_url.to_string(),
            workspace: default_workspace(),
            model: preset.model.to_string(),
            permission_mode: "danger-full-access".to_string(),
            allowed_tools: vec!["read".to_string(), "glob".to_string(), "grep".to_string()],
            keep_open: true,
            prompt: String::new(),
            args: Vec::new(),
            respect_rate_limits: true,
            sandbox_enabled: default_sandbox_enabled(),
            compact_output: false,
            dangerously_skip_permissions: false,
        }
    }
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self::from_preset(provider_presets()[0])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherState {
    profiles: Vec<ProviderProfile>,
    last_selected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ui_selected_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ui_draft: Option<ProviderProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ui_model_search_filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyLauncherConfig {
    workspace: PathBuf,
    model: String,
    permission_mode: String,
    allowed_tools: Vec<String>,
    keep_open: bool,
    prompt: Option<String>,
    openai_api_key: String,
    openai_base_url: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct ProviderPreset {
    kind: ProviderKind,
    name: &'static str,
    base_url: &'static str,
    model: &'static str,
}

#[derive(Debug, Clone)]
struct ToolOption {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    estimated_tokens: u32,
}

#[derive(Debug, Clone)]
struct KnownModel {
    id: &'static str,
    label: &'static str,
    context_window: u32,
    max_output_tokens: u32,
    tool_use_supported: bool,
}

#[derive(Debug, Clone)]
struct ModelView {
    id: String,
    label: String,
    context_window: u32,
    max_output_tokens: u32,
    tool_use_supported: bool,
    from_api: bool,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatModelList {
    data: Vec<OpenAiCompatModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatModel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct HuggingFaceModelSearchResult {
    id: String,
    #[serde(default)]
    siblings: Vec<HuggingFaceSibling>,
}

#[derive(Debug, Deserialize)]
struct HuggingFaceSibling {
    #[serde(alias = "rfilename", alias = "filename")]
    filename: String,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsList {
    models: Vec<OllamaTagModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagModel {
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchProfileFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_kind: Option<ProviderKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keep_open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    respect_rate_limits: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sandbox_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compact_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dangerously_skip_permissions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profiles: Option<Vec<ProviderProfile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_selected: Option<String>,
}

struct LauncherApp {
    exe_dir: PathBuf,
    claw_path: PathBuf,
    legacy_config_path: PathBuf,
    state: LauncherState,
    selected_provider: String,
    draft: ProviderProfile,
    args_text: String,
    selected_tools: BTreeSet<String>,
    models: Vec<ModelView>,
    model_search_filter: String,
    llama_cpp_service_running: Option<bool>,
    cached_git_branch: Option<String>,
    cached_git_branch_workspace: PathBuf,
    service_task: Option<mpsc::Receiver<Result<(bool, String), String>>>,
    download_task: Option<mpsc::Receiver<Result<String, String>>>,
    admin_status: Option<bool>,
    status: String,
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // Make the default window a bit taller so bottom controls aren't truncated
            // on common Windows DPI/scaling settings.
            .with_inner_size([900.0, 860.0])
            .with_icon(load_launcher_window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Claw Launcher",
        options,
        Box::new(|_cc| Ok(Box::new(LauncherApp::new()))),
    )
}

impl LauncherApp {
    fn new() -> Self {
        let exe_dir = current_exe_dir().unwrap_or_else(|_| PathBuf::from("."));
        let legacy_config_path = exe_dir.join("claw-launcher.json");
        let claw_path = exe_dir.join("claw.exe");
        let (state, status) = load_launcher_state(&legacy_config_path);
        log_launcher_event(format!(
            "startup exe_dir='{}' claw_path='{}' status='{}'",
            exe_dir.display(),
            claw_path.display(),
            status
        ));
        let selected_provider = state
            .last_selected
            .clone()
            .filter(|name| {
                state
                    .profiles
                    .iter()
                    .any(|profile| profile.friendly_name == *name)
            })
            .or_else(|| {
                state
                    .profiles
                    .first()
                    .map(|profile| profile.friendly_name.clone())
            })
            .unwrap_or_else(|| NEW_PROVIDER_KEY.to_string());
        let mut app = Self {
            exe_dir,
            claw_path,
            legacy_config_path,
            state,
            selected_provider,
            draft: ProviderProfile::default(),
            args_text: String::new(),
            selected_tools: BTreeSet::new(),
            models: Vec::new(),
            model_search_filter: String::new(),
            llama_cpp_service_running: None,
            cached_git_branch: None,
            cached_git_branch_workspace: PathBuf::new(),
            service_task: None,
            download_task: None,
            admin_status: None,
            status,
        };
        app.load_selected_provider();
        if !app.claw_path.is_file() {
            app.status = format!("Missing {} next to the launcher.", app.claw_path.display());
            log_launcher_event(app.status.clone());
        }
        app
    }

    fn current_profile_index(&self) -> Option<usize> {
        self.state
            .profiles
            .iter()
            .position(|profile| profile.friendly_name == self.selected_provider)
    }

    fn load_selected_provider(&mut self) {
        let profile = self
            .current_profile_index()
            .map(|index| self.state.profiles[index].clone())
            .unwrap_or_default();
        log_launcher_event(format!(
            "load_selected_provider selected='{}' resolved='{}' model='{}' workspace='{}'",
            self.selected_provider,
            profile.friendly_name,
            profile.model,
            profile.workspace.display()
        ));
        self.set_editor_profile(profile);
        self.refresh_models_with_status(false);
    }

    fn set_editor_profile(&mut self, profile: ProviderProfile) {
        self.selected_tools = profile.allowed_tools.iter().cloned().collect();
        self.args_text = profile.args.join("\n");
        self.model_search_filter.clear();
        self.draft = profile;
        self.refresh_cached_git_branch();
    }

    fn sync_editor_profile(&mut self) {
        self.draft.allowed_tools = self.selected_tools.iter().cloned().collect();
        self.draft.args = self
            .args_text
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
    }

    fn filtered_models(&self) -> Vec<&ModelView> {
        let query = self.model_search_filter.trim().to_ascii_lowercase();
        self.models
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.id.to_ascii_lowercase().contains(&query)
                    || model.label.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    fn selected_model(&self) -> Option<&ModelView> {
        self.models
            .iter()
            .find(|model| model.id == self.draft.model)
    }

    fn selected_token_limit(&self) -> (u32, u32) {
        self.selected_model()
            .map(|model| (model.context_window, model.max_output_tokens))
            .unwrap_or_else(|| provider_default_token_limit(self.draft.provider_kind))
    }

    fn apply_preset(&mut self, preset: ProviderPreset) {
        let kind_changed = self.draft.provider_kind != preset.kind;
        self.draft.provider_kind = preset.kind;
        self.draft.base_url = preset.base_url.to_string();
        self.draft.model = preset.model.to_string();
        if kind_changed {
            self.draft.api_key.clear();
            self.llama_cpp_service_running = None;
            self.cached_git_branch_workspace = PathBuf::new();
        }
        if self.selected_provider == NEW_PROVIDER_KEY
            || self.draft.friendly_name.trim().is_empty()
            || provider_presets()
                .iter()
                .any(|candidate| candidate.name == self.draft.friendly_name)
        {
            self.draft.friendly_name = preset.name.to_string();
        }
        self.refresh_models_with_status(true);
    }

    fn refresh_cached_git_branch(&mut self) {
        if self.draft.workspace.as_os_str().is_empty() || !self.draft.workspace.is_dir() {
            self.cached_git_branch = None;
            self.cached_git_branch_workspace = PathBuf::new();
            return;
        }
        if self.cached_git_branch_workspace == self.draft.workspace {
            return;
        }
        self.cached_git_branch_workspace = self.draft.workspace.clone();
        self.cached_git_branch = read_git_branch(&self.draft.workspace);
        log_launcher_event(format!(
            "git_branch workspace='{}' branch='{}'",
            self.draft.workspace.display(),
            self.cached_git_branch
                .clone()
                .unwrap_or_else(|| "(none)".to_string())
        ));
    }

    fn open_api_key_site(&mut self) {
        let url = if matches!(self.draft.provider_kind, ProviderKind::Custom)
            && !self.draft.base_url.trim().is_empty()
        {
            self.draft.base_url.trim().to_string()
        } else {
            self.draft.provider_kind.api_key_url().to_string()
        };
        match webbrowser::open(&url) {
            Ok(()) => {
                self.status = format!(
                    "Opened {} so you can create an API key.",
                    self.draft.provider_kind.label()
                );
            }
            Err(error) => {
                self.status = format!("Failed to open {url}: {error}");
            }
        }
    }

    fn refresh_models_with_status(&mut self, set_status: bool) {
        let refreshed = refresh_models_from_endpoint(
            self.draft.provider_kind,
            &self.draft.base_url,
            &self.draft.api_key,
            &self.model_search_filter,
            &self.draft.huggingface_token,
        );
        match refreshed {
            Ok(models) => {
                self.models = models;
                if set_status {
                    self.status = format!(
                        "Fetched {} model{} from {}.",
                        self.models.len(),
                        if self.models.len() == 1 { "" } else { "s" },
                        self.draft.provider_kind.label()
                    );
                }
            }
            Err(error) => {
                // For local providers, don't keep a stale bundled list (it looks like
                // the refresh worked when it didn't).
                if matches!(
                    self.draft.provider_kind,
                    ProviderKind::Ollama | ProviderKind::LlamaCpp
                ) {
                    self.models.clear();
                }
                if set_status {
                    self.status = format!("Failed to fetch models: {error}");
                }
            }
        }
        if self.models.is_empty()
            && matches!(
                self.draft.provider_kind,
                ProviderKind::Ollama | ProviderKind::LlamaCpp
            )
            && !self.draft.model.trim().is_empty()
        {
            // If the local endpoint can't be queried, keep the UI usable by
            // showing the typed model id as the only option.
            self.models = vec![ModelView {
                id: self.draft.model.trim().to_string(),
                label: "Typed model".to_string(),
                context_window: provider_default_token_limit(self.draft.provider_kind).0,
                max_output_tokens: provider_default_token_limit(self.draft.provider_kind).1,
                tool_use_supported: true,
                from_api: false,
            }];
        }
        if self.models.iter().all(|model| model.id != self.draft.model) {
            if let Some(first) = self.models.first() {
                self.draft.model = first.id.clone();
            }
        }
        self.sanitize_selected_tools();
    }

    fn apply_model_search(&mut self) {
        let matches = self.filtered_models().len();
        self.status = if self.model_search_filter.is_empty() {
            if matches!(
                self.draft.provider_kind,
                ProviderKind::Ollama | ProviderKind::LlamaCpp
            ) {
                "Showing all fetched models for this provider.".to_string()
            } else {
                "Showing all known models for this provider.".to_string()
            }
        } else if matches == 0 {
            if matches!(
                self.draft.provider_kind,
                ProviderKind::Ollama | ProviderKind::LlamaCpp
            ) {
                format!(
                    "No fetched models matched '{}'. You can still launch with the typed model id.",
                    self.model_search_filter
                )
            } else {
                format!(
                    "No known models matched '{}'. You can still launch with the typed model id.",
                    self.model_search_filter
                )
            }
        } else {
            if matches!(
                self.draft.provider_kind,
                ProviderKind::Ollama | ProviderKind::LlamaCpp
            ) {
                format!(
                    "Filtered fetched models with '{}'. {} match{}.",
                    self.model_search_filter,
                    matches,
                    if matches == 1 { "" } else { "es" }
                )
            } else {
                format!(
                    "Filtered known models with '{}'. {} match{}.",
                    self.model_search_filter,
                    matches,
                    if matches == 1 { "" } else { "es" }
                )
            }
        };
    }

    fn model_supports_tools(&self) -> bool {
        self.selected_model()
            .map(|model| model.tool_use_supported)
            .unwrap_or(true)
    }

    fn available_tool_options(&self) -> Vec<ToolOption> {
        if self.model_supports_tools() {
            available_tools()
        } else {
            Vec::new()
        }
    }

    fn sanitize_selected_tools(&mut self) {
        if self.model_supports_tools() {
            let valid_tool_ids = available_tools()
                .into_iter()
                .map(|tool| tool.id.to_string())
                .collect::<BTreeSet<_>>();
            self.selected_tools
                .retain(|tool| valid_tool_ids.contains(tool));
        } else {
            self.selected_tools.clear();
        }
    }

    fn save_provider(&mut self) -> Result<(), String> {
        self.sync_editor_profile();
        let new_name = self.draft.friendly_name.trim();
        if new_name.is_empty() {
            return Err("Enter a provider name first.".to_string());
        }
        let overwrote_existing = self.persist_draft_to_registry(true)?;
        log_launcher_event(format!(
            "save_provider name='{}' kind='{}' workspace='{}' model='{}' prompt_len={} profiles={}",
            self.draft.friendly_name,
            self.draft.provider_kind.label(),
            self.draft.workspace.display(),
            self.draft.model,
            self.draft.prompt.len(),
            self.state.profiles.len()
        ));
        self.status = if overwrote_existing {
            format!(
                "Updated provider '{}' in HKCU\\{}.",
                self.draft.friendly_name, REGISTRY_PATH
            )
        } else {
            format!(
                "Saved provider '{}' to HKCU\\{}.",
                self.draft.friendly_name, REGISTRY_PATH
            )
        };
        Ok(())
    }

    fn delete_provider(&mut self) -> Result<(), String> {
        let Some(index) = self.current_profile_index() else {
            return Err("Select a saved provider to delete.".to_string());
        };
        let removed_name = self.state.profiles[index].friendly_name.clone();
        self.state.profiles.remove(index);
        if let Some(next) = self.state.profiles.first() {
            self.selected_provider = next.friendly_name.clone();
            self.state.last_selected = Some(next.friendly_name.clone());
            self.load_selected_provider();
        } else {
            self.selected_provider = NEW_PROVIDER_KEY.to_string();
            self.state.last_selected = None;
            self.set_editor_profile(ProviderProfile::default());
            self.refresh_models_with_status(false);
        }
        save_launcher_state(&self.state)?;
        if self.draft.workspace.is_dir() {
            let _ =
                write_launch_profile(&self.draft, self.selected_token_limit(), Some(&self.state));
        }
        self.status = format!("Deleted provider '{removed_name}'.");
        Ok(())
    }

    fn launch(&mut self, ctx: &egui::Context) -> Result<(), String> {
        self.sync_editor_profile();
        ensure_runtime_available()?;
        if !self.claw_path.is_file() {
            return Err(format!("Missing {}", self.claw_path.display()));
        }
        if self.draft.workspace.as_os_str().is_empty() || !self.draft.workspace.is_dir() {
            return Err(format!(
                "Workspace does not exist: {}",
                self.draft.workspace.display()
            ));
        }
        if self.draft.provider_kind.requires_api_key() && self.draft.api_key.trim().is_empty() {
            return Err(format!(
                "Enter an API key for '{}' before launching.",
                self.draft.friendly_name
            ));
        }
        if self.draft.model.trim().is_empty() {
            return Err("Choose a model before launching.".to_string());
        }
        self.persist_draft_to_registry(false)?;
        let launch_profile = self
            .state
            .profiles
            .iter()
            .find(|profile| profile.friendly_name == self.selected_provider)
            .cloned()
            .unwrap_or_else(|| self.draft.clone());

        let user_profile = std::env::var("USERPROFILE")
            .map_err(|_| "USERPROFILE is not set on this machine.".to_string())?;

        let effective_model = if matches!(launch_profile.provider_kind, ProviderKind::LlamaCpp) {
            prepare_llama_cpp_server(&launch_profile)?
        } else {
            launch_profile.model.clone()
        };

        write_sandbox_settings_local(&launch_profile.workspace, launch_profile.sandbox_enabled)?;

        let effective_permission_mode = if launch_profile.dangerously_skip_permissions {
            "danger-full-access"
        } else {
            launch_profile.permission_mode.as_str()
        };
        let mut claw_command = format!(
            "& '{}' --model '{}' --permission-mode '{}'",
            powershell_escape(&self.claw_path.display().to_string()),
            powershell_escape(&effective_model),
            powershell_escape(effective_permission_mode)
        );
        if launch_profile.compact_output {
            claw_command.push_str(" --compact");
        }
        if launch_profile.dangerously_skip_permissions {
            claw_command.push_str(" --dangerously-skip-permissions");
        }
        if !launch_profile.allowed_tools.is_empty() {
            claw_command.push_str(&format!(
                " --allowedTools '{}'",
                powershell_escape(&launch_profile.allowed_tools.join(","))
            ));
        }
        for arg in &launch_profile.args {
            claw_command.push_str(&format!(" '{}'", powershell_escape(arg)));
        }

        let powershell_path = resolve_powershell_executable();
        log_launcher_event(format!(
            "launch selected='{}' profile='{}' workspace='{}' powershell='{}' claw='{}' model='{}'",
            self.selected_provider,
            launch_profile.friendly_name,
            launch_profile.workspace.display(),
            powershell_path.display(),
            self.claw_path.display(),
            effective_model
        ));

        let mut command = Command::new(&powershell_path);
        command.current_dir(&launch_profile.workspace);
        command.arg("-NoLogo");
        if launch_profile.keep_open {
            command.arg("-NoExit");
        }
        command.arg("-Command").arg(claw_command);
        if !launch_profile.prompt.trim().is_empty() {
            command.env(LAUNCH_PROMPT_ENV_VAR, launch_profile.prompt.clone());
        }
        command.env("HOME", user_profile);
        for (key, value) in launch_env_vars(&launch_profile) {
            command.env(key, value);
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NEW_CONSOLE);
        }
        command.spawn().map_err(|error| {
            let message = format!(
                "failed to start {} via {}: {}",
                self.claw_path.display(),
                powershell_path.display(),
                error
            );
            log_launcher_event(format!("launch_error {}", message));
            message
        })?;
        self.status = if self.draft.prompt.trim().is_empty() {
            format!(
                "Launching '{}' in {}...",
                launch_profile.friendly_name,
                launch_profile.workspace.display()
            )
        } else {
            format!(
                "Launching '{}' in {}... the prompt will be sent as the first command.",
                launch_profile.friendly_name,
                launch_profile.workspace.display()
            )
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        Ok(())
    }

    fn persist_draft_to_registry(&mut self, require_name: bool) -> Result<bool, String> {
        let resolved_name = self.draft.friendly_name.trim();
        if require_name && resolved_name.is_empty() {
            return Err("Enter a provider name first.".to_string());
        }

        if resolved_name.is_empty() {
            let fallback_name = if self.selected_provider != NEW_PROVIDER_KEY
                && !self.selected_provider.trim().is_empty()
            {
                self.selected_provider.trim().to_string()
            } else {
                self.draft.provider_kind.label().to_string()
            };
            self.draft.friendly_name = fallback_name;
        } else {
            self.draft.friendly_name = resolved_name.to_string();
        }

        let current_index = self.current_profile_index();
        let existing_index = self
            .state
            .profiles
            .iter()
            .enumerate()
            .find_map(|(index, profile)| {
                (profile.friendly_name == self.draft.friendly_name && Some(index) != current_index)
                    .then_some(index)
            });

        let overwrote_existing = current_index.is_some() || existing_index.is_some();
        match current_index.or(existing_index) {
            Some(index) => self.state.profiles[index] = self.draft.clone(),
            None => self.state.profiles.push(self.draft.clone()),
        }
        self.state
            .profiles
            .sort_by(|left, right| left.friendly_name.cmp(&right.friendly_name));
        self.state.last_selected = Some(self.draft.friendly_name.clone());
        self.selected_provider = self.draft.friendly_name.clone();
        save_launcher_state(&self.state)?;
        if self.draft.workspace.is_dir() {
            write_launch_profile(&self.draft, self.selected_token_limit(), Some(&self.state))?;
        }
        Ok(overwrote_existing)
    }

    fn context_estimate(&self) -> Option<(u32, u32, u32)> {
        let model = self.selected_model()?;
        let tool_cost = available_tools()
            .iter()
            .filter(|tool| self.selected_tools.contains(tool.id))
            .map(|tool| tool.estimated_tokens)
            .sum::<u32>();
        let reserved_output = model.max_output_tokens.min(4_096);
        let used = SYSTEM_PROMPT_ESTIMATE + BASE_REQUEST_OVERHEAD + tool_cost + reserved_output;
        let remaining = model.context_window.saturating_sub(used);
        Some((remaining, used, model.context_window))
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.admin_status.is_none() {
            self.admin_status = Some(check_is_running_as_admin());
            if launcher_debug_enabled() {
                log_launcher_event(format!(
                    "admin_status {}",
                    self.admin_status.unwrap_or(false)
                ));
            }
        }
        if let Some(receiver) = self.service_task.as_ref() {
            match receiver.try_recv() {
                Ok(Ok((running, message))) => {
                    self.status = message;
                    self.llama_cpp_service_running = Some(running);
                    self.service_task = None;
                }
                Ok(Err(error)) => {
                    self.status = error;
                    self.service_task = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = "Service operation did not report a result.".to_string();
                    self.service_task = None;
                }
            }
        }
        if let Some(receiver) = self.download_task.as_ref() {
            match receiver.try_recv() {
                Ok(Ok(message)) => {
                    self.status = message;
                    self.download_task = None;
                }
                Ok(Err(error)) => {
                    self.status = error;
                    self.download_task = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = "Download did not report a result.".to_string();
                    self.download_task = None;
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Claw Launcher");
            ui.label("Save reusable provider profiles, switch between them, and launch Claw with the matching workspace, model, and tools.");
            ui.separator();

            ui.group(|ui| {
                ui.heading("Provider");
                let previous_selection = self.selected_provider.clone();
                ui.horizontal(|ui| {
                    ui.label("Profile");
                    egui::ComboBox::from_id_salt("provider-select")
                        .selected_text(if self.selected_provider == NEW_PROVIDER_KEY {
                            "New provider".to_string()
                        } else {
                            self.selected_provider.clone()
                        })
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.selected_provider,
                                NEW_PROVIDER_KEY.to_string(),
                                "New provider",
                            );
                            for profile in &self.state.profiles {
                                ui.selectable_value(
                                    &mut self.selected_provider,
                                    profile.friendly_name.clone(),
                                    profile.friendly_name.clone(),
                                );
                            }
                        });
                    if ui.button("Save Provider").clicked() {
                        if let Err(error) = self.save_provider() {
                            self.status = error;
                        }
                    }
                    if ui
                        .add_enabled(
                            self.current_profile_index().is_some(),
                            egui::Button::new("Delete Provider"),
                        )
                        .clicked()
                    {
                        if let Err(error) = self.delete_provider() {
                            self.status = error;
                        }
                    }
                });
                if self.selected_provider != previous_selection {
                    self.load_selected_provider();
                    self.status = if self.selected_provider == NEW_PROVIDER_KEY {
                        "Creating a new provider profile.".to_string()
                    } else {
                        format!("Loaded provider '{}'.", self.selected_provider)
                    };
                }

                ui.horizontal(|ui| {
                    ui.label("Provider name");
                    ui.add(
                        TextEdit::singleline(&mut self.draft.friendly_name).desired_width(260.0),
                    );
                    ui.label("Type");
                    let previous_kind = self.draft.provider_kind;
                    egui::ComboBox::from_id_salt("provider-kind")
                        .selected_text(self.draft.provider_kind.label())
                        .show_ui(ui, |ui| {
                            for kind in ProviderKind::all() {
                                ui.selectable_value(
                                    &mut self.draft.provider_kind,
                                    kind,
                                    kind.label(),
                                );
                            }
                        });
                    if self.draft.provider_kind != previous_kind {
                        self.draft.api_key.clear();
                        if let Some(preset) = provider_presets()
                            .into_iter()
                            .find(|preset| preset.kind == self.draft.provider_kind)
                        {
                            self.draft.base_url = preset.base_url.to_string();
                            self.draft.model = preset.model.to_string();
                        }
                        self.llama_cpp_service_running = None;
                        self.cached_git_branch_workspace = PathBuf::new();
                        self.refresh_cached_git_branch();
                        self.refresh_models_with_status(true);
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    ui.label("Quick start");
                    for preset in provider_presets() {
                        let is_selected = self.draft.provider_kind == preset.kind;
                        let button = if is_selected {
                            egui::Button::new(preset.name)
                                .fill(Color32::from_rgb(46, 160, 67))
                                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(24, 100, 45)))
                        } else {
                            egui::Button::new(preset.name)
                        };
                        if ui.add(button).clicked() {
                            self.apply_preset(preset);
                        }
                    }
                });

                ui.horizontal(|ui| {
                    if matches!(self.draft.provider_kind, ProviderKind::LlamaCpp) {
                        ui.label("HF token");
                        ui.add(
                            TextEdit::singleline(&mut self.draft.huggingface_token)
                                .password(true)
                                .desired_width(420.0)
                                .hint_text("Optional (for gated/private models)"),
                        );
                        if ui.button("Refresh Models").clicked() {
                            self.refresh_models_with_status(true);
                        }
                    } else {
                        ui.label("API key");
                        ui.add(
                            TextEdit::singleline(&mut self.draft.api_key)
                                .password(true)
                                .desired_width(420.0),
                        );
                        let needs_api_key = self.draft.provider_kind.requires_api_key()
                            && self.draft.api_key.trim().is_empty();
                        let action_label = if needs_api_key {
                            "Get API Key"
                        } else {
                            "Refresh Models"
                        };
                        if ui.button(action_label).clicked() {
                            if needs_api_key {
                                self.open_api_key_site();
                            } else {
                                self.refresh_models_with_status(true);
                            }
                        }
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Base URL");
                    ui.add(TextEdit::singleline(&mut self.draft.base_url).desired_width(420.0));
                });

                if matches!(self.draft.provider_kind, ProviderKind::LlamaCpp) {
                    ui.horizontal(|ui| {
                        ui.label("llama-server.exe");
                        let mut display = if self.draft.llama_cpp_server_path.as_os_str().is_empty()
                        {
                            format!("(auto: .\\{LLAMA_CPP_BIN_DIR_NAME}\\llama-server.exe)")
                        } else {
                            self.draft.llama_cpp_server_path.display().to_string()
                        };
                        ui.add_enabled(
                            false,
                            TextEdit::singleline(&mut display).desired_width(420.0),
                        );
                        if ui.button("Browse").clicked() {
                            if let Some(file) = rfd::FileDialog::new()
                                .set_directory(&self.draft.workspace)
                                .add_filter("Executable", &["exe"])
                                .pick_file()
                            {
                                let lowered =
                                    file.file_name().and_then(|name| name.to_str()).unwrap_or("");
                                if lowered.eq_ignore_ascii_case("llama-server.exe") {
                                    self.draft.llama_cpp_server_path = file.clone();
                                    self.llama_cpp_service_running = None;
                                    self.status = format!(
                                        "Selected llama.cpp server: {}",
                                        file.display()
                                    );
                                } else {
                                    self.status = "Pick llama-server.exe (from your llama.cpp bin folder)."
                                        .to_string();
                                }
                            }
                        }
                        if ui.button("Clear").clicked() {
                            self.draft.llama_cpp_server_path = PathBuf::new();
                            self.llama_cpp_service_running = None;
                            self.status = format!(
                                "Cleared llama-server.exe override (using .\\{LLAMA_CPP_BIN_DIR_NAME}\\llama-server.exe)."
                            );
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Model");
                        let cached = resolve_llama_cpp_cached_model(&self.draft).ok();
                        let downloaded = cached.as_ref().is_some_and(|path| path.is_file());
                        let status = if downloaded {
                            "Downloaded"
                        } else if cached.is_some() {
                            "Not downloaded"
                        } else {
                            "Choose model"
                        };
                        ui.label(status);

                        let can_download = cached.is_some() && self.download_task.is_none();
                        if ui.add_enabled(can_download, egui::Button::new("Download model")).clicked()
                        {
                            self.status = "Downloading llama.cpp model...".to_string();
                            let profile = self.draft.clone();
                            let (tx, rx) = mpsc::channel();
                            self.download_task = Some(rx);
                            std::thread::spawn(move || {
                                let result = (|| {
                                    let (repo_id, filename) = parse_llama_cpp_spec(&profile.model)
                                        .or_else(|| parse_bare_spec(&profile.model))
                                        .ok_or_else(|| {
                                            "Choose a llama.cpp model first (expected 'llama.cpp/<repo_id>::<filename.gguf>')."
                                                .to_string()
                                        })?;
                                    let path = ensure_llama_cpp_model_downloaded(
                                        &profile.workspace,
                                        &repo_id,
                                        &filename,
                                        &profile.huggingface_token,
                                    )?;
                                    Ok::<_, String>(format!(
                                        "Downloaded model to {}",
                                        path.display()
                                    ))
                                })();
                                let _ = tx.send(result);
                            });
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Service");
                        let is_admin = self.admin_status.unwrap_or(false);
                        if !is_admin {
                            ui.colored_label(
                                Color32::from_rgb(215, 58, 73),
                                "Admin required to install/uninstall",
                            );
                            if ui.button("Restart as Admin").clicked() {
                                let exe = std::env::current_exe()
                                    .ok()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_default();
                                if exe.is_empty() {
                                    self.status = "Could not resolve launcher exe path.".to_string();
                                } else {
                                    let mut command = Command::new(resolve_powershell_executable());
                                    command.arg("-NoLogo").arg("-Command").arg(format!(
                                        "Start-Process -FilePath '{}' -Verb RunAs",
                                        exe.replace('\'', "''")
                                    ));
                                    if command.spawn().is_ok() {
                                        self.status = "Requested elevation. Approve the UAC prompt to reopen the launcher as Administrator.".to_string();
                                    } else {
                                        self.status = "Failed to request elevation.".to_string();
                                    }
                                }
                            }
                            if ui.button("Recheck").clicked() {
                                self.admin_status = Some(check_is_running_as_admin());
                            }
                        }
                        let running = self.llama_cpp_service_running.unwrap_or_else(|| {
                            if launcher_debug_enabled() {
                                log_launcher_event(format!(
                                    "service_cache_miss provider='{}' kind='{}'",
                                    self.draft.friendly_name,
                                    self.draft.provider_kind.label()
                                ));
                            }
                            let running = llama_cpp_service_is_running();
                            self.llama_cpp_service_running = Some(running);
                            running
                        });
                        let status = if running { "Running" } else { "Not running" };
                        ui.label(status);

                        if running {
                            if ui
                                .add_enabled(
                                    is_admin && self.service_task.is_none(),
                                    egui::Button::new("Uninstall"),
                                )
                                .clicked()
                            {
                                self.status = "Uninstalling llama.cpp Windows service...".to_string();
                                let (tx, rx) = mpsc::channel();
                                self.service_task = Some(rx);
                                std::thread::spawn(move || {
                                    let result = uninstall_llama_cpp_service()
                                        .map(|_| (false, "Uninstalled llama.cpp Windows service.".to_string()));
                                    let _ = tx.send(result);
                                });
                            }
                            if ui
                                .add_enabled(
                                    is_admin && self.service_task.is_none(),
                                    egui::Button::new("Repair"),
                                )
                                .clicked()
                            {
                                self.status = "Repairing llama.cpp Windows service...".to_string();
                                let exe_dir = self.exe_dir.clone();
                                let profile = self.draft.clone();
                                let (tx, rx) = mpsc::channel();
                                self.service_task = Some(rx);
                                std::thread::spawn(move || {
                                    let result = repair_llama_cpp_service(&exe_dir, &profile)
                                        .map(|_| (true, "Repaired llama.cpp Windows service.".to_string()));
                                    let _ = tx.send(result);
                                });
                            }
                        } else if ui
                            .add_enabled(
                                is_admin
                                    && self.service_task.is_none()
                                    && self.download_task.is_none(),
                                egui::Button::new("Install"),
                            )
                            .clicked()
                        {
                            self.status = "Installing llama.cpp Windows service...".to_string();
                            let exe_dir = self.exe_dir.clone();
                            let profile = self.draft.clone();
                            let (tx, rx) = mpsc::channel();
                            self.service_task = Some(rx);
                            std::thread::spawn(move || {
                                let result = install_llama_cpp_service(&exe_dir, &profile)
                                    .map(|_| (true, "Installed llama.cpp Windows service.".to_string()));
                                let _ = tx.send(result);
                            });
                        }
                    });
                }

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.draft.respect_rate_limits, "Respect rate limits");
                    if ui.button("Clear credentials").clicked() {
                        self.draft.api_key.clear();
                        self.status = "Cleared API key.".to_string();
                    }
                    if ui.button("Fetch rate limits").clicked() {
                        self.status = "Fetching rate limits...".to_string();
                    }
                });
            });

            ui.add_space(8.0);
                ui.group(|ui| {
                    ui.heading("Workspace And Launch");
                    ui.horizontal(|ui| {
                        ui.label("Project folder");
                        let mut workspace_display = self.draft.workspace.display().to_string();
                        ui.add_enabled(
                            false,
                            TextEdit::singleline(&mut workspace_display).desired_width(420.0),
                        );
                        if ui.button("Select Folder").clicked() {
                            if let Some(folder) = rfd::FileDialog::new()
                                .set_directory(&self.draft.workspace)
                                .pick_folder()
                            {
                                if let Some(root) = resolve_git_root(&folder) {
                                    self.draft.workspace = root.clone();
                                    self.cached_git_branch_workspace = PathBuf::new();
                                    self.refresh_cached_git_branch();
                                    let branch = self
                                        .cached_git_branch
                                        .clone()
                                        .unwrap_or_else(|| "unknown".to_string());
                                    self.status = format!(
                                        "Selected git project: {} (branch {}).",
                                        root.display(),
                                        branch
                                    );
                                } else {
                                    self.draft.workspace = folder.clone();
                                    self.cached_git_branch_workspace = PathBuf::new();
                                    self.refresh_cached_git_branch();
                                    self.status = format!(
                                        "Selected folder (not a git repo): {}.",
                                        folder.display()
                                    );
                                }
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        let text = self
                            .cached_git_branch
                            .clone()
                            .map(|branch| format!("Git branch: {branch}"))
                            .unwrap_or_else(|| "Git branch: (not a git repo)".to_string());
                        ui.small(text);
                        if ui.button("Refresh branch").clicked() {
                            self.cached_git_branch_workspace = PathBuf::new();
                            self.refresh_cached_git_branch();
                            self.status = "Refreshed git branch.".to_string();
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Permission");
                        ui.add_enabled_ui(!self.draft.dangerously_skip_permissions, |ui| {
                            egui::ComboBox::from_id_salt("permission-mode")
                            .selected_text(self.draft.permission_mode.clone())
                            .show_ui(ui, |ui| {
                                for mode in ["danger-full-access", "workspace-write", "read-only"] {
                                    ui.selectable_value(
                                        &mut self.draft.permission_mode,
                                        mode.to_string(),
                                        mode,
                                    );
                                }
                            });
                    });
                    if self.draft.dangerously_skip_permissions {
                        ui.small("Forced: danger-full-access");
                    }
                    ui.checkbox(&mut self.draft.keep_open, "Keep terminal open");
                    ui.checkbox(&mut self.draft.sandbox_enabled, "Sandbox mode");
                });

                ui.label("Prompt");
                ui.add(
                    TextEdit::multiline(&mut self.draft.prompt)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3),
                );

                ui.horizontal_wrapped(|ui| {
                    ui.label("Options");
                    if ui
                        .checkbox(&mut self.draft.compact_output, "Compact output")
                        .clicked()
                    {
                        // no-op; state stored in profile
                    }
                    if ui
                        .checkbox(
                            &mut self.draft.dangerously_skip_permissions,
                            "Skip permission checks",
                        )
                        .clicked()
                        && self.draft.dangerously_skip_permissions
                    {
                        self.draft.permission_mode = "danger-full-access".to_string();
                    }
                });

                ui.collapsing("Advanced", |ui| {
                    ui.label("Extra args (one per line)");
                    ui.add(
                        TextEdit::multiline(&mut self.args_text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(2),
                    );
                });
            });

            ui.add_space(8.0);
            ui.group(|ui| {
                ui.heading("Model And Tools");
                ui.horizontal(|ui| {
                    ui.label("Search");
                    let search_response = ui.add(
                        TextEdit::singleline(&mut self.model_search_filter)
                            .hint_text("Filter models (Enter refreshes)")
                            .desired_width(280.0),
                    );
                    let enter_pressed = search_response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Search/Refresh").clicked() || enter_pressed {
                        self.refresh_models_with_status(true);
                    }
                    if search_response.changed() {
                        self.apply_model_search();
                    }
                    let filtered_models = self.filtered_models();
                    let mut newly_selected_model = None;
                    egui::ComboBox::from_id_salt("model-select")
                        .selected_text(if self.model_search_filter.trim().is_empty() {
                            format!("Choose model ({})", filtered_models.len())
                        } else {
                            format!("Choose model ({})", filtered_models.len())
                        })
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for model in filtered_models {
                                let text = format!(
                                    "{}  [{} ctx / {} out{}]",
                                    model.id,
                                    model.context_window,
                                    model.max_output_tokens,
                                    if model.tool_use_supported {
                                        ", tool use"
                                    } else {
                                        ", no tool use"
                                    }
                                );
                                if ui.selectable_label(false, text).clicked() {
                                    newly_selected_model = Some(model.id.clone());
                                }
                            }
                        });
                    if ui.button("Refresh models").clicked() {
                        self.refresh_models_with_status(true);
                    }
                    if let Some(model_id) = newly_selected_model {
                        self.draft.model = model_id;
                        self.sanitize_selected_tools();
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Model");
                    let model_response =
                        ui.add(TextEdit::singleline(&mut self.draft.model).desired_width(280.0));
                    if model_response.changed() {
                        self.sanitize_selected_tools();
                    }
                });

                if let Some(model) = self.selected_model() {
                    ui.label(format!(
                        "Model details: {} | context {} | max output {} | {}{}",
                        model.label,
                        model.context_window,
                        model.max_output_tokens,
                        if model.tool_use_supported {
                            "tool use supported"
                        } else {
                            "tool use not supported"
                        },
                        if model.from_api {
                            " | listed by provider API"
                        } else {
                            " | bundled metadata"
                        }
                    ));
                }

                ui.separator();
                ui.label("Available tools");
                let tool_options = self.available_tool_options();
                if tool_options.is_empty() {
                    ui.small("The selected model does not advertise tool support for this provider.");
                } else {
                    for tool in tool_options {
                        let mut checked = self.selected_tools.contains(tool.id);
                        let text = format!("{} ({})", tool.label, tool.description);
                        if ui.checkbox(&mut checked, text).changed() {
                            if checked {
                                self.selected_tools.insert(tool.id.to_string());
                            } else {
                                self.selected_tools.remove(tool.id);
                            }
                        }
                    }
                }

                if let Some((remaining, used, total)) = self.context_estimate() {
                    let ratio = if total == 0 {
                        0.0
                    } else {
                        remaining as f32 / total as f32
                    };
                    let (label, color) = if ratio > 0.55 {
                        ("Green", Color32::from_rgb(46, 160, 67))
                    } else if ratio > 0.30 {
                        ("Amber", Color32::from_rgb(210, 153, 34))
                    } else {
                        ("Red", Color32::from_rgb(215, 58, 73))
                    };
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "Estimated context headroom: {} tokens free of {} total ({} used). Indicator: {}",
                            remaining, total, used, label
                        ))
                        .color(color),
                    );
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save Provider").clicked() {
                    if let Err(error) = self.save_provider() {
                        self.status = error;
                    }
                }
                if ui.button("Launch").clicked() {
                    match self.save_provider().and_then(|_| self.launch(ctx)) {
                        Ok(()) => {}
                        Err(error) => self.status = error,
                    }
                }
            });

            ui.separator();
            ui.label(&self.status);
            ui.small(format!("Provider registry: HKCU\\{}", REGISTRY_PATH));
            ui.small(format!(
                "Legacy config import source: {}",
                self.legacy_config_path.display()
            ));
            ui.small(format!("Launcher directory: {}", self.exe_dir.display()));
        });
    }
}

fn load_launcher_window_icon() -> Arc<IconData> {
    let icon_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("assets")
        .join("openclaw.ico");
    let icon = fs::read(icon_path)
        .ok()
        .and_then(|bytes| image::load_from_memory(&bytes).ok())
        .map(|image| {
            let rgba = image.into_rgba8();
            let (width, height) = rgba.dimensions();
            IconData {
                rgba: rgba.into_raw(),
                width,
                height,
            }
        })
        .unwrap_or_default();
    Arc::new(icon)
}

fn default_workspace() -> PathBuf {
    std::env::current_dir()
        .or_else(|_| current_exe_dir())
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn resolve_git_root(path: &Path) -> Option<PathBuf> {
    let output = git_command()
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let trimmed = root.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn read_git_branch(path: &Path) -> Option<String> {
    let output = git_command()
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8(output.stdout).ok()?;
    let trimmed = branch.trim();
    if trimmed.is_empty() || trimmed == "HEAD" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn provider_presets() -> [ProviderPreset; 6] {
    [
        ProviderPreset {
            kind: ProviderKind::Groq,
            name: "Groq",
            base_url: "https://api.groq.com/openai/v1",
            model: "llama-3.3-70b-versatile",
        },
        ProviderPreset {
            kind: ProviderKind::OpenRouter,
            name: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            model: "openai/gpt-oss-120b",
        },
        ProviderPreset {
            kind: ProviderKind::GoogleAiStudio,
            name: "Google AI Studio",
            base_url: "https://generativelanguage.googleapis.com/v1beta",
            model: "gemini-2.0-flash",
        },
        ProviderPreset {
            kind: ProviderKind::Ollama,
            name: "Ollama (Local)",
            base_url: "http://localhost:11434/v1",
            model: "llama3",
        },
        ProviderPreset {
            kind: ProviderKind::LlamaCpp,
            name: "llama.cpp (Local)",
            base_url: "http://127.0.0.1:8080/v1",
            model: "",
        },
        ProviderPreset {
            kind: ProviderKind::Custom,
            name: "Custom",
            base_url: "",
            model: "",
        },
    ]
}

fn starter_profiles() -> Vec<ProviderProfile> {
    provider_presets()
        .into_iter()
        .map(ProviderProfile::from_preset)
        .collect()
}

fn load_launcher_state(legacy_config_path: &Path) -> (LauncherState, String) {
    let sidecar_path = default_workspace().join(LAUNCH_PROFILE_FILE_NAME);
    let registry_state = read_registry_state();
    if let Some(state) = registry_state
        .clone()
        .map(|state| hydrate_state_from_sidecar(state, &sidecar_path))
    {
        log_launcher_event(format!(
            "load_launcher_state source=registry path='{}' profiles={} last_selected='{}'",
            sidecar_path.display(),
            state.profiles.len(),
            state.last_selected.clone().unwrap_or_default()
        ));
        return (
            state,
            "Loaded provider profiles from the registry.".to_string(),
        );
    }
    if let Some(state) = load_state_from_sidecar(&sidecar_path, None) {
        log_launcher_event(format!(
            "load_launcher_state source=sidecar path='{}' profiles={} last_selected='{}'",
            sidecar_path.display(),
            state.profiles.len(),
            state.last_selected.clone().unwrap_or_default()
        ));
        return (
            state,
            format!("Loaded provider profiles from {}.", sidecar_path.display()),
        );
    }
    if let Some(legacy) = load_legacy_config(legacy_config_path) {
        let profile = ProviderProfile {
            friendly_name: "Imported provider".to_string(),
            provider_kind: ProviderKind::Custom,
            api_key: legacy.openai_api_key,
            huggingface_token: String::new(),
            llama_cpp_server_path: PathBuf::new(),
            base_url: legacy.openai_base_url,
            workspace: legacy.workspace,
            model: legacy.model,
            permission_mode: legacy.permission_mode,
            allowed_tools: legacy.allowed_tools,
            keep_open: legacy.keep_open,
            prompt: legacy.prompt.unwrap_or_default(),
            args: legacy.args,
            respect_rate_limits: true,
            sandbox_enabled: default_sandbox_enabled(),
            compact_output: false,
            dangerously_skip_permissions: false,
        };
        return (
            LauncherState {
                profiles: vec![profile],
                last_selected: Some("Imported provider".to_string()),
                ui_selected_provider: None,
                ui_draft: None,
                ui_model_search_filter: None,
            },
            "Imported the legacy launcher config. Save once to migrate it into the registry."
                .to_string(),
        );
    }
    (
        LauncherState {
            profiles: starter_profiles(),
            last_selected: Some("Groq".to_string()),
            ui_selected_provider: None,
            ui_draft: None,
            ui_model_search_filter: None,
        },
        "Created starter provider profiles. Add your API key and workspace, then save.".to_string(),
    )
}

fn read_registry_state() -> Option<LauncherState> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey(REGISTRY_PATH).ok()?;
    let body: String = key.get_value(REGISTRY_STATE_VALUE).ok()?;
    let parsed = serde_json::from_str(&body).ok();
    log_launcher_event(format!(
        "read_registry_state success={} bytes={}",
        parsed.is_some(),
        body.len()
    ));
    parsed
}

fn load_state_from_sidecar(
    path: &Path,
    registry_state: Option<&LauncherState>,
) -> Option<LauncherState> {
    let body = fs::read_to_string(path).ok()?;
    let sidecar = serde_json::from_str::<LaunchProfileFile>(&body).ok()?;

    let merged_profiles = if let Some(profiles) = sidecar.profiles.clone() {
        profiles
    } else if let Some(profile) = profile_from_sidecar(&sidecar, registry_state) {
        vec![profile]
    } else {
        registry_state?.profiles.clone()
    };

    let last_selected = sidecar
        .last_selected
        .clone()
        .or_else(|| sidecar.provider_name.clone())
        .or_else(|| registry_state.and_then(|state| state.last_selected.clone()))
        .or_else(|| {
            merged_profiles
                .first()
                .map(|profile| profile.friendly_name.clone())
        });

    Some(LauncherState {
        profiles: merge_profiles_with_registry(merged_profiles, registry_state),
        last_selected,
        ui_selected_provider: registry_state.and_then(|state| state.ui_selected_provider.clone()),
        ui_draft: registry_state.and_then(|state| state.ui_draft.clone()),
        ui_model_search_filter: registry_state
            .and_then(|state| state.ui_model_search_filter.clone()),
    })
}

fn hydrate_state_from_sidecar(mut state: LauncherState, path: &Path) -> LauncherState {
    let Ok(body) = fs::read_to_string(path) else {
        return state;
    };
    let Ok(sidecar) = serde_json::from_str::<LaunchProfileFile>(&body) else {
        return state;
    };

    if let Some(last_selected) = sidecar
        .last_selected
        .clone()
        .or_else(|| sidecar.provider_name.clone())
    {
        if state
            .profiles
            .iter()
            .any(|profile| profile.friendly_name == last_selected)
        {
            state.last_selected = Some(last_selected);
        }
    }

    if let Some(profile) = profile_from_sidecar(&sidecar, Some(&state)) {
        if let Some(existing) = state
            .profiles
            .iter_mut()
            .find(|candidate| candidate.friendly_name == profile.friendly_name)
        {
            *existing = merge_profile_with_registry_fallback(profile, existing.clone());
        } else {
            state.profiles.push(profile);
            state
                .profiles
                .sort_by(|left, right| left.friendly_name.cmp(&right.friendly_name));
        }
    }

    state
}

fn profile_from_sidecar(
    sidecar: &LaunchProfileFile,
    registry_state: Option<&LauncherState>,
) -> Option<ProviderProfile> {
    let name = sidecar
        .provider_name
        .clone()
        .or_else(|| registry_state.and_then(|state| state.last_selected.clone()))?;
    let fallback = registry_state.and_then(|state| {
        state
            .profiles
            .iter()
            .find(|profile| profile.friendly_name == name)
            .cloned()
    });
    Some(ProviderProfile {
        friendly_name: name,
        provider_kind: sidecar
            .provider_kind
            .or_else(|| fallback.as_ref().map(|profile| profile.provider_kind))
            .unwrap_or(ProviderKind::Custom),
        api_key: sidecar
            .api_key
            .clone()
            .or_else(|| fallback.as_ref().map(|profile| profile.api_key.clone()))
            .unwrap_or_default(),
        huggingface_token: fallback
            .as_ref()
            .map(|profile| profile.huggingface_token.clone())
            .unwrap_or_default(),
        llama_cpp_server_path: fallback
            .as_ref()
            .map(|profile| profile.llama_cpp_server_path.clone())
            .unwrap_or_default(),
        base_url: sidecar
            .base_url
            .clone()
            .or_else(|| fallback.as_ref().map(|profile| profile.base_url.clone()))
            .unwrap_or_default(),
        workspace: sidecar
            .workspace
            .clone()
            .or_else(|| fallback.as_ref().map(|profile| profile.workspace.clone()))
            .unwrap_or_else(default_workspace),
        model: sidecar
            .model
            .clone()
            .or_else(|| fallback.as_ref().map(|profile| profile.model.clone()))
            .unwrap_or_default(),
        permission_mode: sidecar
            .permission_mode
            .clone()
            .or_else(|| {
                fallback
                    .as_ref()
                    .map(|profile| profile.permission_mode.clone())
            })
            .unwrap_or_else(|| "danger-full-access".to_string()),
        allowed_tools: sidecar
            .allowed_tools
            .clone()
            .or_else(|| {
                fallback
                    .as_ref()
                    .map(|profile| profile.allowed_tools.clone())
            })
            .unwrap_or_default(),
        keep_open: sidecar
            .keep_open
            .or_else(|| fallback.as_ref().map(|profile| profile.keep_open))
            .unwrap_or(true),
        prompt: sidecar
            .prompt
            .clone()
            .or_else(|| fallback.as_ref().map(|profile| profile.prompt.clone()))
            .unwrap_or_default(),
        args: sidecar
            .args
            .clone()
            .or_else(|| fallback.as_ref().map(|profile| profile.args.clone()))
            .unwrap_or_default(),
        respect_rate_limits: sidecar
            .respect_rate_limits
            .or_else(|| fallback.as_ref().map(|profile| profile.respect_rate_limits))
            .unwrap_or(true),
        sandbox_enabled: sidecar
            .sandbox_enabled
            .or_else(|| fallback.as_ref().map(|profile| profile.sandbox_enabled))
            .unwrap_or_else(default_sandbox_enabled),
        compact_output: sidecar
            .compact_output
            .or_else(|| fallback.as_ref().map(|profile| profile.compact_output))
            .unwrap_or(false),
        dangerously_skip_permissions: sidecar
            .dangerously_skip_permissions
            .or_else(|| {
                fallback
                    .as_ref()
                    .map(|profile| profile.dangerously_skip_permissions)
            })
            .unwrap_or(false),
    })
}

fn merge_profiles_with_registry(
    sidecar_profiles: Vec<ProviderProfile>,
    registry_state: Option<&LauncherState>,
) -> Vec<ProviderProfile> {
    let Some(registry_state) = registry_state else {
        return sidecar_profiles;
    };
    sidecar_profiles
        .into_iter()
        .map(|profile| {
            let Some(registry_profile) = registry_state
                .profiles
                .iter()
                .find(|candidate| candidate.friendly_name == profile.friendly_name)
            else {
                return profile;
            };
            merge_profile_with_registry_fallback(profile, registry_profile.clone())
        })
        .collect()
}

fn merge_profile_with_registry_fallback(
    profile: ProviderProfile,
    registry_profile: ProviderProfile,
) -> ProviderProfile {
    ProviderProfile {
        friendly_name: profile.friendly_name,
        provider_kind: profile.provider_kind,
        api_key: if profile.api_key.trim().is_empty() {
            registry_profile.api_key
        } else {
            profile.api_key
        },
        huggingface_token: if profile.huggingface_token.trim().is_empty() {
            registry_profile.huggingface_token
        } else {
            profile.huggingface_token
        },
        llama_cpp_server_path: if profile.llama_cpp_server_path.as_os_str().is_empty() {
            registry_profile.llama_cpp_server_path
        } else {
            profile.llama_cpp_server_path
        },
        base_url: if profile.base_url.trim().is_empty() {
            registry_profile.base_url
        } else {
            profile.base_url
        },
        workspace: if profile.workspace.as_os_str().is_empty() {
            registry_profile.workspace
        } else {
            profile.workspace
        },
        model: if profile.model.trim().is_empty() {
            registry_profile.model
        } else {
            profile.model
        },
        permission_mode: if profile.permission_mode.trim().is_empty() {
            registry_profile.permission_mode
        } else {
            profile.permission_mode
        },
        allowed_tools: if profile.allowed_tools.is_empty() {
            registry_profile.allowed_tools
        } else {
            profile.allowed_tools
        },
        keep_open: profile.keep_open,
        prompt: if profile.prompt.trim().is_empty() {
            registry_profile.prompt
        } else {
            profile.prompt
        },
        args: if profile.args.is_empty() {
            registry_profile.args
        } else {
            profile.args
        },
        respect_rate_limits: profile.respect_rate_limits,
        sandbox_enabled: profile.sandbox_enabled,
        compact_output: profile.compact_output,
        dangerously_skip_permissions: profile.dangerously_skip_permissions,
    }
}

fn save_launcher_state(state: &LauncherState) -> Result<(), String> {
    let body = serde_json::to_string_pretty(state)
        .map_err(|error| format!("failed to serialize launcher state: {error}"))?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(REGISTRY_PATH)
        .map_err(|error| format!("failed to open HKCU\\{}: {error}", REGISTRY_PATH))?;
    key.set_value(REGISTRY_STATE_VALUE, &body)
        .map_err(|error| {
            let message = format!("failed to write launcher state to the registry: {error}");
            log_launcher_event(format!("save_launcher_state error={message}"));
            message
        })?;
    log_launcher_event(format!(
        "save_launcher_state profiles={} last_selected='{}' bytes={}",
        state.profiles.len(),
        state.last_selected.clone().unwrap_or_default(),
        body.len()
    ));
    Ok(())
}

fn write_launch_profile(
    profile: &ProviderProfile,
    token_limit: (u32, u32),
    state: Option<&LauncherState>,
) -> Result<(), String> {
    let launch_profile = LaunchProfileFile {
        provider_name: Some(profile.friendly_name.clone()),
        provider_kind: Some(profile.provider_kind),
        api_key: Some(profile.api_key.clone()),
        model: Some(profile.model.clone()),
        base_url: Some(profile.base_url.clone()),
        workspace: Some(profile.workspace.clone()),
        permission_mode: Some(profile.permission_mode.clone()),
        allowed_tools: Some(profile.allowed_tools.clone()),
        keep_open: Some(profile.keep_open),
        prompt: Some(profile.prompt.clone()),
        args: Some(profile.args.clone()),
        context_window_tokens: Some(token_limit.0),
        max_output_tokens: Some(token_limit.1),
        respect_rate_limits: Some(profile.respect_rate_limits),
        sandbox_enabled: Some(profile.sandbox_enabled),
        compact_output: Some(profile.compact_output),
        dangerously_skip_permissions: Some(profile.dangerously_skip_permissions),
        profiles: state.map(|state| state.profiles.clone()),
        last_selected: Some(profile.friendly_name.clone()),
    };
    let body = serde_json::to_string_pretty(&launch_profile)
        .map_err(|error| format!("failed to serialize launch profile: {error}"))?;
    let path = profile.workspace.join(LAUNCH_PROFILE_FILE_NAME);
    fs::write(&path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn write_sandbox_settings_local(workspace: &Path, enabled: bool) -> Result<(), String> {
    let config_dir = workspace.join(".claw");
    fs::create_dir_all(&config_dir)
        .map_err(|error| format!("failed to create {}: {error}", config_dir.display()))?;
    let path = config_dir.join(SANDBOX_SETTINGS_LOCAL_FILE_NAME);

    let mut root = if path.is_file() {
        let body = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if body.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str::<serde_json::Value>(&body)
                .map_err(|error| format!("failed to parse {}: {error}", path.display()))?
        }
    } else {
        serde_json::json!({})
    };

    let object = root
        .as_object_mut()
        .ok_or_else(|| format!("{} must be a JSON object", path.display()))?;
    let sandbox = object
        .entry("sandbox")
        .or_insert_with(|| serde_json::json!({}));
    let sandbox_object = sandbox
        .as_object_mut()
        .ok_or_else(|| format!("{}.sandbox must be a JSON object", path.display()))?;
    sandbox_object.insert("enabled".to_string(), serde_json::Value::Bool(enabled));

    let body = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("failed to serialize {}: {error}", path.display()))?;
    fs::write(&path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn load_legacy_config(path: &Path) -> Option<LegacyLauncherConfig> {
    let body = fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn current_exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("failed to resolve parent directory for {}", exe.display()))
}

fn launcher_log_path() -> Option<PathBuf> {
    current_exe_dir()
        .ok()
        .map(|dir| dir.join(LAUNCHER_LOG_FILE_NAME))
}

fn log_launcher_event(message: impl AsRef<str>) {
    let Some(path) = launcher_log_path() else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{timestamp}] {}", message.as_ref());
    }
}

fn launcher_debug_enabled() -> bool {
    std::env::var("CLAW_LAUNCHER_DEBUG")
        .ok()
        .map(|value| {
            let lowered = value.trim().to_ascii_lowercase();
            lowered == "1" || lowered == "true" || lowered == "yes" || lowered == "on"
        })
        .unwrap_or(false)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LlamaServiceConfig {
    server_exe: PathBuf,
    model_path: PathBuf,
    host: String,
    port: u16,
}

fn service_wrapper_exe_path(exe_dir: &Path) -> PathBuf {
    exe_dir.join(LLAMA_CPP_SERVICE_WRAPPER_EXE)
}

fn resolve_service_wrapper_exe_path(exe_dir: &Path) -> PathBuf {
    let primary = service_wrapper_exe_path(exe_dir);
    if primary.is_file() {
        return primary;
    }
    exe_dir.join(LLAMA_CPP_SERVICE_WRAPPER_EXE_FALLBACK)
}

fn service_wrapper_config_path(exe_dir: &Path) -> PathBuf {
    exe_dir.join(LLAMA_CPP_SERVICE_CONFIG_FILENAME)
}

fn check_is_running_as_admin() -> bool {
    let mut command = Command::new("net.exe");
    command.arg("session");
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command.status().is_ok_and(|status| status.success())
}

fn resolve_powershell_executable() -> PathBuf {
    if let Some(windir) = std::env::var_os("WINDIR") {
        let candidate = PathBuf::from(windir)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("powershell.exe")
}

fn ensure_runtime_available() -> Result<(), String> {
    let required = ["VCRUNTIME140.dll", "ucrtbase.dll"];
    let missing = required
        .into_iter()
        .filter(|dll| !can_find_runtime_dll(dll))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let _ = webbrowser::open(VC_REDIST_URL);
    Err(format!(
        "Missing Windows runtime component(s): {}. Opened the Microsoft VC++ redistributable download page.",
        missing.join(", ")
    ))
}

fn can_find_runtime_dll(name: &str) -> bool {
    let Some(windir) = std::env::var_os("WINDIR").map(PathBuf::from) else {
        return false;
    };
    ["System32", "SysWOW64"]
        .into_iter()
        .any(|folder| windir.join(folder).join(name).is_file())
}

fn launch_env_vars(profile: &ProviderProfile) -> Vec<(&'static str, String)> {
    let mut vars = vec![
        ("CLAW_MODEL", profile.model.clone()),
        ("CLAW_PROVIDER_NAME", profile.friendly_name.clone()),
        (
            "CLAW_RESPECT_RATE_LIMITS",
            profile.respect_rate_limits.to_string(),
        ),
    ];
    match profile.provider_kind {
        ProviderKind::GoogleAiStudio => {
            vars.push(("GOOGLE_API_KEY", profile.api_key.clone()));
            vars.push(("GOOGLE_BASE_URL", profile.base_url.clone()));
        }
        ProviderKind::Groq => {
            vars.push(("GROQ_API_KEY", profile.api_key.clone()));
            vars.push(("GROQ_BASE_URL", profile.base_url.clone()));
        }
        ProviderKind::Ollama | ProviderKind::LlamaCpp => {
            vars.push(("OPENAI_BASE_URL", profile.base_url.clone()));
            if matches!(profile.provider_kind, ProviderKind::LlamaCpp)
                && !profile.huggingface_token.trim().is_empty()
            {
                vars.push(("HF_TOKEN", profile.huggingface_token.clone()));
            }
        }
        _ => {
            vars.push(("OPENAI_API_KEY", profile.api_key.clone()));
            vars.push(("OPENAI_BASE_URL", profile.base_url.clone()));
        }
    }
    vars
}

fn provider_default_token_limit(provider_kind: ProviderKind) -> (u32, u32) {
    match provider_kind {
        ProviderKind::Groq => (131_072, 8_192),
        ProviderKind::OpenRouter => (131_072, 16_384),
        ProviderKind::GoogleAiStudio => (131_072, 16_384),
        ProviderKind::Ollama => (32_768, 4_096),
        ProviderKind::LlamaCpp => (32_768, 4_096),
        ProviderKind::Custom => (131_072, 16_384),
    }
}

fn available_tools() -> Vec<ToolOption> {
    vec![
        ToolOption {
            id: "read",
            label: "read",
            description: "Read files",
            estimated_tokens: 450,
        },
        ToolOption {
            id: "glob",
            label: "glob",
            description: "Find files by pattern",
            estimated_tokens: 250,
        },
        ToolOption {
            id: "grep",
            label: "grep",
            description: "Search file contents",
            estimated_tokens: 350,
        },
        ToolOption {
            id: "write",
            label: "write",
            description: "Write files",
            estimated_tokens: 450,
        },
        ToolOption {
            id: "edit",
            label: "edit",
            description: "Edit files",
            estimated_tokens: 450,
        },
        ToolOption {
            id: "bash",
            label: "bash",
            description: "Run shell commands",
            estimated_tokens: 700,
        },
    ]
}

fn known_models() -> Vec<KnownModel> {
    vec![
        KnownModel {
            id: "llama-3.3-70b-versatile",
            label: "Llama 3.3 70B Versatile",
            context_window: 131_072,
            max_output_tokens: 32_768,
            tool_use_supported: true,
        },
        KnownModel {
            id: "openai/gpt-oss-120b",
            label: "GPT-OSS 120B",
            context_window: 131_072,
            max_output_tokens: 65_536,
            tool_use_supported: true,
        },
        KnownModel {
            id: "openai/gpt-oss-20b",
            label: "GPT-OSS 20B",
            context_window: 131_072,
            max_output_tokens: 65_536,
            tool_use_supported: true,
        },
        KnownModel {
            id: "qwen/qwen3-32b",
            label: "Qwen 3 32B",
            context_window: 131_072,
            max_output_tokens: 16_384,
            tool_use_supported: true,
        },
        KnownModel {
            id: "meta-llama/llama-4-scout-17b-16e-instruct",
            label: "Llama 4 Scout 17B",
            context_window: 131_072,
            max_output_tokens: 8_192,
            tool_use_supported: true,
        },
        KnownModel {
            id: "groq/compound",
            label: "Compound",
            context_window: 131_072,
            max_output_tokens: 8_192,
            tool_use_supported: false,
        },
        KnownModel {
            id: "gemini-2.0-flash",
            label: "Gemini 2.0 Flash",
            context_window: 1_048_576,
            max_output_tokens: 8_192,
            tool_use_supported: true,
        },
        KnownModel {
            id: "gemini-2.0-flash-lite",
            label: "Gemini 2.0 Flash-Lite",
            context_window: 1_048_576,
            max_output_tokens: 8_192,
            tool_use_supported: true,
        },
        KnownModel {
            id: "gpt-4.1-mini",
            label: "GPT-4.1 Mini",
            context_window: 1_047_576,
            max_output_tokens: 32_768,
            tool_use_supported: true,
        },
        KnownModel {
            id: "gpt-4.1",
            label: "GPT-4.1",
            context_window: 1_047_576,
            max_output_tokens: 32_768,
            tool_use_supported: true,
        },
        KnownModel {
            id: "grok-3-mini",
            label: "Grok 3 Mini",
            context_window: 131_072,
            max_output_tokens: 16_384,
            tool_use_supported: true,
        },
        KnownModel {
            id: "claude-sonnet-4-5",
            label: "Claude Sonnet 4.5",
            context_window: 200_000,
            max_output_tokens: 8_192,
            tool_use_supported: true,
        },
        KnownModel {
            id: LEGACY_DEFAULT_MODEL,
            label: "Legacy Default",
            context_window: 131_072,
            max_output_tokens: 32_768,
            tool_use_supported: true,
        },
    ]
}

fn refresh_models_from_endpoint(
    provider_kind: ProviderKind,
    base_url: &str,
    api_key: &str,
    query: &str,
    huggingface_token: &str,
) -> Result<Vec<ModelView>, String> {
    if matches!(provider_kind, ProviderKind::LlamaCpp) {
        return refresh_llama_cpp_models_from_huggingface(query, huggingface_token);
    }
    let mut by_id = if matches!(provider_kind, ProviderKind::Ollama) {
        BTreeMap::new()
    } else {
        known_models()
            .into_iter()
            .filter(model_is_applicable)
            .filter(|model| model_matches_provider(model.id, provider_kind))
            .map(|model| {
                (
                    model.id.to_string(),
                    ModelView {
                        id: model.id.to_string(),
                        label: model.label.to_string(),
                        context_window: model.context_window,
                        max_output_tokens: model.max_output_tokens,
                        tool_use_supported: model.tool_use_supported,
                        from_api: false,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>()
    };

    let remote_models = fetch_models(provider_kind, base_url, api_key)?;
    for remote_model in remote_models {
        if !model_matches_provider(&remote_model.id, provider_kind)
            || !remote_model_is_applicable(&remote_model.id)
        {
            continue;
        }
        let entry = by_id
            .entry(remote_model.id.clone())
            .or_insert_with(|| ModelView {
                id: remote_model.id.clone(),
                label: known_model_label(&remote_model.id)
                    .unwrap_or_else(|| remote_model.id.clone()),
                context_window: known_model_context_window(&remote_model.id).unwrap_or(131_072),
                max_output_tokens: known_model_max_output_tokens(&remote_model.id).unwrap_or(8_192),
                tool_use_supported: true,
                from_api: true,
            });
        entry.from_api = true;
    }

    let mut models = by_id.into_values().collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(models)
}

fn fetch_models(
    provider_kind: ProviderKind,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<OpenAiCompatModel>, String> {
    if !provider_kind.supports_remote_models() {
        return Err("provider does not expose a compatible /models endpoint".to_string());
    }
    let trimmed_url = base_url.trim();
    if trimmed_url.is_empty() {
        return Err("base URL is empty".to_string());
    }
    if provider_kind.requires_api_key() && api_key.trim().is_empty() {
        return Err("API key is empty".to_string());
    }

    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;

    // Ollama can expose models via OpenAI-compat `/v1/models` *or* the native
    // `/api/tags`. Try both to keep the launcher flexible.
    if matches!(provider_kind, ProviderKind::Ollama) {
        let url = format!("{}/models", trimmed_url.trim_end_matches('/'));
        let response = client
            .get(&url)
            .send()
            .map_err(|error| format!("failed to fetch models: {error}"))?;
        let models_status = response.status();
        if models_status.is_success() {
            if let Ok(payload) = response.json::<OpenAiCompatModelList>() {
                return Ok(payload.data);
            }
            // Fall through to /api/tags if the response shape isn't compatible.
        }

        let base = trimmed_url.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        let tags_url = format!("{}/api/tags", base);
        let response = client
            .get(&tags_url)
            .send()
            .map_err(|error| format!("failed to fetch Ollama tags: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "GET {url} -> {models_status}; GET {tags_url} -> {}",
                response.status()
            ));
        }
        let payload = response
            .json::<OllamaTagsList>()
            .map_err(|error| format!("failed to parse Ollama tags response: {error}"))?;
        return Ok(payload
            .models
            .into_iter()
            .map(|model| OpenAiCompatModel { id: model.name })
            .collect());
    }

    let mut request = client.get(format!("{}/models", trimmed_url.trim_end_matches('/')));
    if provider_kind.requires_api_key() {
        request = request.bearer_auth(api_key.trim());
    }
    let response = request
        .send()
        .map_err(|error| format!("failed to fetch models: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("models request failed with {}", response.status()));
    }
    response
        .json::<OpenAiCompatModelList>()
        .map(|payload| payload.data)
        .map_err(|error| format!("failed to parse models response: {error}"))
}

fn huggingface_token() -> Option<String> {
    for key in [
        "HF_TOKEN",
        "HUGGINGFACE_HUB_TOKEN",
        "HUGGINGFACE_TOKEN",
        "HUGGINGFACE_API_TOKEN",
    ] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn huggingface_token_override(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn huggingface_token_for_request(override_value: &str) -> Option<String> {
    huggingface_token_override(override_value).or_else(huggingface_token)
}

fn refresh_llama_cpp_models_from_huggingface(
    query: &str,
    huggingface_token_override: &str,
) -> Result<Vec<ModelView>, String> {
    let query = query.trim();
    let query = if query.is_empty() {
        // Searching Hugging Face without a query is too broad. Default to a
        // known GGUF publisher to keep "Refresh models" fast and relevant.
        "LiquidAI gguf"
    } else {
        query
    };

    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;

    let limit = "20".to_string();
    let mut request = client.get("https://huggingface.co/api/models").query(&[
        ("search", query),
        ("limit", limit.as_str()),
        ("full", "true"),
        ("sort", "downloads"),
        ("direction", "-1"),
    ]);
    if let Some(token) = huggingface_token_for_request(huggingface_token_override) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|error| format!("failed to query Hugging Face models: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Hugging Face model search failed with {}",
            response.status()
        ));
    }
    let results = response
        .json::<Vec<HuggingFaceModelSearchResult>>()
        .map_err(|error| format!("failed to parse Hugging Face response: {error}"))?;

    let (context_window, max_output_tokens) = provider_default_token_limit(ProviderKind::LlamaCpp);
    let mut models = Vec::new();
    for model in results {
        for sibling in model.siblings {
            if !sibling.filename.to_ascii_lowercase().ends_with(".gguf") {
                continue;
            }
            let id = format!("llama.cpp/{}::{}", model.id, sibling.filename);
            let label = format!("{} ({})", model.id, sibling.filename);
            models.push(ModelView {
                id,
                label,
                context_window,
                max_output_tokens,
                tool_use_supported: true,
                from_api: true,
            });
            if models.len() >= 120 {
                break;
            }
        }
        if models.len() >= 120 {
            break;
        }
    }

    if models.is_empty() {
        return Err(format!(
            "No GGUF files found for '{query}'. Try a different search (example: 'qwen gguf')."
        ));
    }

    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(models)
}

fn parse_llama_cpp_spec(model: &str) -> Option<(String, String)> {
    let trimmed = model.trim();
    let spec = trimmed.strip_prefix(LLAMA_CPP_MODEL_PREFIX)?;
    let mut parts = spec.splitn(2, "::");
    let repo_id = parts.next()?.trim();
    let filename = parts.next()?.trim();
    if repo_id.is_empty() || filename.is_empty() {
        return None;
    }
    Some((repo_id.to_string(), filename.to_string()))
}

fn parse_bare_spec(model: &str) -> Option<(String, String)> {
    let trimmed = model.trim();
    let mut parts = trimmed.splitn(2, "::");
    let repo_id = parts.next()?.trim();
    let filename = parts.next()?.trim();
    if repo_id.is_empty() || filename.is_empty() {
        return None;
    }
    Some((repo_id.to_string(), filename.to_string()))
}

fn find_llama_cpp_bin_dir(start: &Path) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("LLAMA_CPP_BIN_DIR") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if path.is_dir() {
                return Some(path);
            }
        }
    }

    let mut cursor = Some(start);
    while let Some(dir) = cursor {
        let candidate = dir.join(LLAMA_CPP_BIN_DIR_NAME);
        if candidate.is_dir() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

fn base_url_models_endpoint(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

fn llama_cpp_is_ready(client: &Client, base_url: &str) -> bool {
    let url = base_url_models_endpoint(base_url);
    client
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .is_ok_and(|resp| resp.status().is_success())
}

fn parse_host_port(base_url: &str) -> Result<(String, u16), String> {
    let trimmed = base_url.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
        "llama.cpp base URL must include a scheme (example: http://127.0.0.1:8080/v1)".to_string()
    })?;
    let authority = rest.split('/').next().unwrap_or("").trim();
    if authority.is_empty() {
        return Err("llama.cpp base URL is missing a host".to_string());
    }

    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443u16
    } else {
        80u16
    };

    let mut parts = authority.rsplitn(2, ':');
    let last = parts.next().unwrap_or("");
    let maybe_host = parts.next();
    if let Some(host) = maybe_host {
        let port = last
            .parse::<u16>()
            .map_err(|_| format!("invalid port in llama.cpp base URL: '{last}'"))?;
        Ok((host.to_string(), port))
    } else {
        Ok((last.to_string(), default_port))
    }
}

fn resolve_llama_cpp_server_exe(profile: &ProviderProfile) -> Result<PathBuf, String> {
    if profile.llama_cpp_server_path.is_file() {
        return Ok(profile.llama_cpp_server_path.clone());
    }

    let bin_dir = find_llama_cpp_bin_dir(&profile.workspace).ok_or_else(|| {
        format!(
            "could not find {LLAMA_CPP_BIN_DIR_NAME}. Set LLAMA_CPP_BIN_DIR, browse for llama-server.exe, or place the directory in your project folder."
        )
    })?;
    let server = bin_dir.join("llama-server.exe");
    if !server.is_file() {
        return Err(format!(
            "llama.cpp server binary not found at {}. Browse for llama-server.exe or set LLAMA_CPP_BIN_DIR.",
            server.display()
        ));
    }
    Ok(server)
}

fn ensure_llama_cpp_model_downloaded(
    repo_root: &Path,
    repo_id: &str,
    filename: &str,
    huggingface_token_override: &str,
) -> Result<PathBuf, String> {
    let safe_repo = repo_id.replace('/', "__");
    let dest_dir = repo_root
        .join(".claw")
        .join("llama.cpp")
        .join("models")
        .join(safe_repo);
    fs::create_dir_all(&dest_dir)
        .map_err(|error| format!("failed to create llama.cpp model cache dir: {error}"))?;
    let dest_path = dest_dir.join(filename);
    if dest_path.is_file() {
        return Ok(dest_path);
    }

    let url = format!("https://huggingface.co/{repo_id}/resolve/main/{filename}?download=true");
    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;

    let part_path = dest_path.with_extension("gguf.part");
    let token = huggingface_token_for_request(huggingface_token_override);

    for attempt in 1..=3 {
        if launcher_debug_enabled() {
            log_launcher_event(format!(
                "hf_download attempt={} url='{}' dest='{}'",
                attempt,
                url,
                dest_path.display()
            ));
        }

        let mut request = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(600));
        if let Some(token) = token.clone() {
            request = request.bearer_auth(token);
        }

        let response = request.send();
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                let message = format!("failed to download GGUF from Hugging Face: {error}");
                if attempt == 3 {
                    return Err(message);
                }
                std::thread::sleep(std::time::Duration::from_secs(attempt * 2));
                continue;
            }
        };

        if !response.status().is_success() {
            return Err(format!(
                "download failed: GET {url} -> {} (if this is a gated model, enter an HF token)",
                response.status()
            ));
        }

        let mut output = match fs::File::create(&part_path) {
            Ok(file) => file,
            Err(error) => return Err(format!("failed to write model: {error}")),
        };

        let mut written: u64 = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        let mut download_error: Option<String> = None;
        loop {
            let read = match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) => {
                    download_error = Some(format!(
                        "failed to write GGUF to disk after {written} bytes: {error}"
                    ));
                    break;
                }
            };
            if let Err(error) = output.write_all(&buffer[..read]) {
                download_error = Some(format!(
                    "failed to write GGUF to disk after {written} bytes: {error}"
                ));
                break;
            }
            written += read as u64;
        }

        if let Some(message) = download_error {
            let _ = fs::remove_file(&part_path);
            if launcher_debug_enabled() {
                log_launcher_event(format!(
                    "hf_download_error attempt={} written={} error='{}'",
                    attempt, written, message
                ));
            }
            if attempt == 3 {
                return Err(message);
            }
            std::thread::sleep(std::time::Duration::from_secs(attempt * 2));
            continue;
        }

        output
            .flush()
            .map_err(|error| format!("failed to flush GGUF to disk: {error}"))?;
        fs::rename(&part_path, &dest_path)
            .map_err(|error| format!("failed to finalize GGUF download: {error}"))?;
        return Ok(dest_path);
    }

    Ok(dest_path)
}

fn llama_cpp_cached_model_path(repo_root: &Path, repo_id: &str, filename: &str) -> PathBuf {
    let safe_repo = repo_id.replace('/', "__");
    repo_root
        .join(".claw")
        .join("llama.cpp")
        .join("models")
        .join(safe_repo)
        .join(filename)
}

fn resolve_llama_cpp_cached_model(profile: &ProviderProfile) -> Result<PathBuf, String> {
    let (repo_id, filename) = parse_llama_cpp_spec(&profile.model)
        .or_else(|| parse_bare_spec(&profile.model))
        .ok_or_else(|| {
            "Choose a llama.cpp model first (expected 'llama.cpp/<repo_id>::<filename.gguf>')."
                .to_string()
        })?;
    if !filename.to_ascii_lowercase().ends_with(".gguf") {
        return Err(format!(
            "llama.cpp model spec must reference a .gguf file (got '{filename}')."
        ));
    }
    Ok(llama_cpp_cached_model_path(
        &profile.workspace,
        &repo_id,
        &filename,
    ))
}

fn start_llama_cpp_server(
    base_url: &str,
    server_exe: &Path,
    model_path: &Path,
) -> Result<(), String> {
    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;
    if llama_cpp_is_ready(&client, base_url) {
        return Ok(());
    }

    if !server_exe.is_file() {
        return Err(format!(
            "llama.cpp server binary not found at {}. Browse for llama-server.exe or set LLAMA_CPP_BIN_DIR.",
            server_exe.display()
        ));
    }

    let (host, port) = parse_host_port(base_url)?;

    let bin_dir = server_exe
        .parent()
        .ok_or_else(|| format!("invalid llama-server.exe path: {}", server_exe.display()))?;

    let mut command = Command::new(server_exe);
    command.current_dir(bin_dir);
    command.arg("-m").arg(model_path);
    command.arg("--host").arg(host);
    command.arg("--port").arg(port.to_string());

    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    command.env(
        "PATH",
        format!("{};{}", bin_dir.display(), existing_path.to_string_lossy()),
    );
    command.spawn().map_err(|error| {
        format!(
            "failed to start llama.cpp server at {}: {error}",
            server_exe.display()
        )
    })?;

    let url = base_url_models_endpoint(base_url);
    for _ in 0..40 {
        if llama_cpp_is_ready(&client, base_url) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    Err(format!(
        "llama.cpp server did not become ready at {url} after waiting."
    ))
}

fn resolve_llama_cpp_model_id(base_url: &str) -> Result<String, String> {
    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;
    let url = base_url_models_endpoint(base_url);
    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .map_err(|error| format!("failed to query llama.cpp /models endpoint: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GET {url} -> {}", response.status()));
    }
    let payload = response
        .json::<OpenAiCompatModelList>()
        .map_err(|error| format!("failed to parse /models response: {error}"))?;
    payload
        .data
        .into_iter()
        .next()
        .map(|model| model.id)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "llama.cpp /models returned no model ids".to_string())
}

fn sc_output_to_string(output: &std::process::Output) -> String {
    let mut parts = Vec::new();
    if !output.stdout.is_empty() {
        parts.push(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    if !output.stderr.is_empty() {
        parts.push(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn sc_failed_code(output_text: &str) -> Option<u32> {
    let failed = output_text.find("FAILED ")?;
    let after = &output_text[(failed + "FAILED ".len())..];
    let mut digits = String::new();
    for ch in after.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

fn run_sc(args: &[String]) -> Result<std::process::Output, String> {
    let mut command = Command::new("sc.exe");
    command.args(args);

    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    if launcher_debug_enabled() {
        log_launcher_event(format!("sc.exe args={:?}", args));
    }

    command
        .output()
        .map_err(|error| format!("failed to run sc.exe: {error}"))
}

fn llama_cpp_service_exists() -> bool {
    let args = ["query".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    run_sc(&args).is_ok_and(|output| output.status.success())
}

fn llama_cpp_service_is_running() -> bool {
    let args = ["query".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    let Ok(output) = run_sc(&args) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let combined = sc_output_to_string(&output);
    let running = combined.to_ascii_uppercase().contains("RUNNING");
    if launcher_debug_enabled() {
        log_launcher_event(format!(
            "service_query name='{}' running={}",
            LLAMA_CPP_SERVICE_NAME, running
        ));
    }
    running
}

fn uninstall_llama_cpp_service_with_wait() -> Result<(), String> {
    if !llama_cpp_service_exists() {
        return Ok(());
    }

    let stop_args = ["stop".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    let _ = run_sc(&stop_args);

    let delete_args = ["delete".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    let deleted = run_sc(&delete_args)?;
    if !deleted.status.success() {
        let output = sc_output_to_string(&deleted);
        return Err(format!(
            "failed to uninstall service (try running the launcher as Administrator): {}",
            output
        ));
    }

    // SCM can keep the service "marked for deletion" until all handles close.
    // Wait a bit so recreate works in one click.
    for _ in 0..40 {
        if !llama_cpp_service_exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    Ok(())
}

fn install_llama_cpp_service(exe_dir: &Path, profile: &ProviderProfile) -> Result<(), String> {
    let base_url = profile.base_url.trim();
    if base_url.is_empty() {
        return Err("llama.cpp base URL is empty".to_string());
    }

    let wrapper_exe = resolve_service_wrapper_exe_path(exe_dir);
    if !wrapper_exe.is_file() {
        return Err(format!(
            "Missing {} next to the launcher. Rebuild/reinstall the launcher bundle.",
            wrapper_exe.display()
        ));
    }

    let server_exe = resolve_llama_cpp_server_exe(profile)?;
    let model_path = resolve_llama_cpp_cached_model(profile)?;
    if !model_path.is_file() {
        return Err(format!(
            "Model is not downloaded yet (expected {}). Click 'Download model' first.",
            model_path.display()
        ));
    }

    let (host, port) = parse_host_port(base_url)?;

    let config = LlamaServiceConfig {
        server_exe: server_exe.clone(),
        model_path: model_path.clone(),
        host,
        port,
    };
    let config_path = service_wrapper_config_path(exe_dir);
    let body = serde_json::to_vec_pretty(&config)
        .map_err(|error| format!("failed to serialize service config: {error}"))?;
    fs::write(&config_path, body)
        .map_err(|error| format!("failed to write {}: {error}", config_path.display()))?;

    let wrapper_binpath = format!("\"{}\"", wrapper_exe.display());

    if launcher_debug_enabled() {
        log_launcher_event(format!(
            "service_install wrapper='{}' server='{}' model='{}' base_url='{}'",
            wrapper_exe.display(),
            server_exe.display(),
            model_path.display(),
            base_url
        ));
    }

    if llama_cpp_service_exists() {
        let _ = uninstall_llama_cpp_service_with_wait();
        // Update the existing service to point at the wrapper (and new config).
        let config_args = [
            "config".to_string(),
            LLAMA_CPP_SERVICE_NAME.to_string(),
            "binPath=".to_string(),
            wrapper_binpath.clone(),
            "start=".to_string(),
            "auto".to_string(),
        ];
        let configured = run_sc(&config_args)?;
        if !configured.status.success() {
            return Err(format!(
                "failed to configure Windows service (try running the launcher as Administrator): {}",
                sc_output_to_string(&configured)
            ));
        }
    } else {
        let create_args = [
            "create".to_string(),
            LLAMA_CPP_SERVICE_NAME.to_string(),
            "binPath=".to_string(),
            wrapper_binpath.clone(),
            "start=".to_string(),
            "auto".to_string(),
            "DisplayName=".to_string(),
            "Claw llama.cpp server".to_string(),
        ];
        let create = run_sc(&create_args)?;
        if !create.status.success() {
            let output = sc_output_to_string(&create);
            if sc_failed_code(&output) == Some(1072) {
                return Err(format!(
                    "service is marked for deletion; wait a few seconds and try again. Details: {output}"
                ));
            }
            return Err(format!(
                "failed to create Windows service (try running the launcher as Administrator): {}",
                output
            ));
        }
    }

    let start_args = ["start".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    let start = run_sc(&start_args)?;
    if !start.status.success() {
        return Err(format!(
            "service installed but failed to start: {}",
            sc_output_to_string(&start)
        ));
    }

    Ok(())
}

fn uninstall_llama_cpp_service() -> Result<(), String> {
    if !llama_cpp_service_exists() {
        return Ok(());
    }
    let stop_args = ["stop".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    let _ = run_sc(&stop_args);
    let delete_args = ["delete".to_string(), LLAMA_CPP_SERVICE_NAME.to_string()];
    let deleted = run_sc(&delete_args)?;
    if !deleted.status.success() {
        return Err(format!(
            "failed to uninstall service (try running the launcher as Administrator): {}",
            sc_output_to_string(&deleted)
        ));
    }
    Ok(())
}

fn repair_llama_cpp_service(exe_dir: &Path, profile: &ProviderProfile) -> Result<(), String> {
    if launcher_debug_enabled() {
        log_launcher_event("service_repair requested");
    }
    let _ = uninstall_llama_cpp_service_with_wait();
    install_llama_cpp_service(exe_dir, profile)
}

fn prepare_llama_cpp_server(profile: &ProviderProfile) -> Result<String, String> {
    let base_url = profile.base_url.trim();
    if base_url.is_empty() {
        return Err("llama.cpp base URL is empty".to_string());
    }

    let (repo_id, filename) = parse_llama_cpp_spec(&profile.model)
        .or_else(|| parse_bare_spec(&profile.model))
        .ok_or_else(|| {
            "expected model spec 'llama.cpp/<repo_id>::<filename.gguf>' (or '<repo_id>::<filename.gguf>')"
                .to_string()
        })?;

    if !filename.to_ascii_lowercase().ends_with(".gguf") {
        return Err(format!(
            "llama.cpp model spec must reference a .gguf file (got '{filename}')."
        ));
    }

    let server_exe = resolve_llama_cpp_server_exe(profile)?;
    let repo_root = profile.workspace.clone();

    let model_path = ensure_llama_cpp_model_downloaded(
        &repo_root,
        &repo_id,
        &filename,
        &profile.huggingface_token,
    )?;
    start_llama_cpp_server(base_url, &server_exe, &model_path)?;

    resolve_llama_cpp_model_id(base_url).or_else(|_| Ok("llama.cpp".to_string()))
}

fn model_matches_provider(model_id: &str, provider_kind: ProviderKind) -> bool {
    let lowered = model_id.to_ascii_lowercase();
    match provider_kind {
        ProviderKind::Groq => {
            matches!(
                model_id,
                "llama-3.3-70b-versatile"
                    | "meta-llama/llama-4-scout-17b-16e-instruct"
                    | "qwen/qwen3-32b"
            ) || lowered.starts_with("llama-")
                || lowered.starts_with("mixtral")
                || lowered.starts_with("gemma")
                || lowered.starts_with("qwen/")
                || lowered.starts_with("meta-llama/")
                || lowered.starts_with("moonshotai/")
                || lowered.starts_with("deepseek-")
        }
        ProviderKind::OpenRouter => lowered.contains('/'),
        ProviderKind::GoogleAiStudio => lowered.starts_with("gemini-"),
        ProviderKind::Ollama => true,
        ProviderKind::LlamaCpp => true,
        ProviderKind::Custom => true,
    }
}

fn model_is_applicable(model: &KnownModel) -> bool {
    model.tool_use_supported && model.id != LEGACY_DEFAULT_MODEL
}

fn remote_model_is_applicable(model_id: &str) -> bool {
    !model_id.trim().is_empty()
}

fn known_model_label(model_id: &str) -> Option<String> {
    known_models()
        .into_iter()
        .find(|model| model.id == model_id)
        .map(|model| model.label.to_string())
}

fn known_model_context_window(model_id: &str) -> Option<u32> {
    known_models()
        .into_iter()
        .find(|model| model.id == model_id)
        .map(|model| model.context_window)
}

fn known_model_max_output_tokens(model_id: &str) -> Option<u32> {
    known_models()
        .into_iter()
        .find(|model| model.id == model_id)
        .map(|model| model.max_output_tokens)
}

fn powershell_escape(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn cwd_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn default_workspace_uses_current_directory() {
        let _guard = cwd_lock();
        let root = std::env::temp_dir().join(format!(
            "claw-launcher-default-workspace-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("workspace fixture");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("switch cwd");

        assert_eq!(default_workspace(), root);

        std::env::set_current_dir(previous).expect("restore cwd");
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
