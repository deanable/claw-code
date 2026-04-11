#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

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
const LEGACY_DEFAULT_MODEL: &str = "llama-3.3-70b-versatile";
const NEW_PROVIDER_KEY: &str = "__new_provider__";
const LAUNCH_PROFILE_FILE_NAME: &str = ".claw-launch.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ProviderKind {
    Groq,
    OpenAi,
    OpenRouter,
    DashScope,
    Xai,
    Anthropic,
    Ollama,
    Custom,
}

impl ProviderKind {
    fn all() -> [Self; 8] {
        [
            Self::Groq,
            Self::OpenAi,
            Self::OpenRouter,
            Self::DashScope,
            Self::Xai,
            Self::Anthropic,
            Self::Ollama,
            Self::Custom,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq",
            Self::OpenAi => "OpenAI",
            Self::OpenRouter => "OpenRouter",
            Self::DashScope => "DashScope",
            Self::Xai => "xAI",
            Self::Anthropic => "Anthropic",
            Self::Ollama => "Ollama",
            Self::Custom => "Custom",
        }
    }

    fn supports_remote_models(self) -> bool {
        !matches!(self, Self::Anthropic)
    }

    fn requires_api_key(self) -> bool {
        !matches!(self, Self::Ollama)
    }

    fn api_key_url(self) -> &'static str {
        match self {
            Self::Groq => "https://console.groq.com/keys",
            Self::OpenAi => "https://platform.openai.com/api-keys",
            Self::OpenRouter => "https://openrouter.ai/keys",
            Self::DashScope => "https://dashscope.console.aliyun.com/apiKey",
            Self::Xai => "https://console.x.ai",
            Self::Anthropic => "https://console.anthropic.com/settings/keys",
            Self::Ollama => "https://ollama.com/download",
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
    base_url: String,
    workspace: PathBuf,
    model: String,
    permission_mode: String,
    allowed_tools: Vec<String>,
    keep_open: bool,
    prompt: String,
    args: Vec<String>,
}

impl ProviderProfile {
    fn from_preset(preset: ProviderPreset) -> Self {
        Self {
            friendly_name: preset.name.to_string(),
            provider_kind: preset.kind,
            api_key: String::new(),
            base_url: preset.base_url.to_string(),
            workspace: default_workspace(),
            model: preset.model.to_string(),
            permission_mode: "danger-full-access".to_string(),
            allowed_tools: vec!["read".to_string(), "glob".to_string(), "grep".to_string()],
            keep_open: true,
            prompt: String::new(),
            args: Vec::new(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchProfileFile {
    provider_name: String,
    provider_kind: ProviderKind,
    model: String,
    base_url: String,
    workspace: PathBuf,
    context_window_tokens: u32,
    max_output_tokens: u32,
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
    status: String,
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 780.0])
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
            status,
        };
        app.load_selected_provider();
        if !app.claw_path.is_file() {
            app.status = format!("Missing {} next to the launcher.", app.claw_path.display());
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
        self.set_editor_profile(profile);
        self.refresh_models_with_status(false);
    }

    fn set_editor_profile(&mut self, profile: ProviderProfile) {
        self.selected_tools = profile.allowed_tools.iter().cloned().collect();
        self.args_text = profile.args.join("\n");
        self.model_search_filter.clear();
        self.draft = profile;
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
        self.models = initial_models(
            self.draft.provider_kind,
            &self.draft.base_url,
            &self.draft.api_key,
        );
        if self.models.iter().all(|model| model.id != self.draft.model) {
            if let Some(first) = self.models.first() {
                self.draft.model = first.id.clone();
            }
        }
        self.sanitize_selected_tools();
        if set_status {
            self.status = if self.draft.api_key.trim().is_empty()
                && self.draft.provider_kind.requires_api_key()
            {
                format!(
                    "Add an API key for {} to load live models.",
                    self.draft.provider_kind.label()
                )
            } else {
                format!("Loaded models for {}.", self.draft.provider_kind.label())
            };
        }
    }

    fn apply_model_search(&mut self) {
        self.model_search_filter = self.draft.model.trim().to_string();
        let matches = self.filtered_models().len();
        self.status = if self.model_search_filter.is_empty() {
            "Showing all known models for this provider.".to_string()
        } else if matches == 0 {
            format!(
                "No known models matched '{}'. You can still launch with the typed model id.",
                self.model_search_filter
            )
        } else {
            format!(
                "Filtered known models with '{}'. {} match{}.",
                self.model_search_filter,
                matches,
                if matches == 1 { "" } else { "es" }
            )
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

        let current_index = self.current_profile_index();
        let duplicate = self
            .state
            .profiles
            .iter()
            .enumerate()
            .any(|(index, profile)| {
                profile.friendly_name == new_name && Some(index) != current_index
            });
        if duplicate {
            return Err(format!("A provider named '{new_name}' already exists."));
        }

        self.draft.friendly_name = new_name.to_string();
        match current_index {
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
            let _ = write_launch_profile(&self.draft, self.selected_token_limit());
        }
        self.status = format!(
            "Saved provider '{}' to HKCU\\{}.",
            self.draft.friendly_name, REGISTRY_PATH
        );
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
        write_launch_profile(&self.draft, self.selected_token_limit())?;

        let user_profile = std::env::var("USERPROFILE")
            .map_err(|_| "USERPROFILE is not set on this machine.".to_string())?;

        let mut claw_command = format!(
            "& '{}' --model '{}' --permission-mode '{}'",
            powershell_escape(&self.claw_path.display().to_string()),
            powershell_escape(&self.draft.model),
            powershell_escape(&self.draft.permission_mode)
        );
        if !self.draft.allowed_tools.is_empty() {
            claw_command.push_str(&format!(
                " --allowedTools '{}'",
                powershell_escape(&self.draft.allowed_tools.join(","))
            ));
        }
        if !self.draft.prompt.trim().is_empty() {
            claw_command.push_str(&format!(
                " prompt '{}'",
                powershell_escape(self.draft.prompt.trim())
            ));
        }
        for arg in &self.draft.args {
            claw_command.push_str(&format!(" '{}'", powershell_escape(arg)));
        }

        let mut command = Command::new("powershell");
        command.current_dir(&self.draft.workspace);
        command.arg("-NoLogo");
        if self.draft.keep_open {
            command.arg("-NoExit");
        }
        command.arg("-Command").arg(claw_command);
        command.env("HOME", user_profile);
        for (key, value) in launch_env_vars(&self.draft) {
            command.env(key, value);
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NEW_CONSOLE);
        }
        command
            .spawn()
            .map_err(|error| format!("failed to start {}: {error}", self.claw_path.display()))?;
        self.status = format!("Launching '{}'...", self.draft.friendly_name);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        Ok(())
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
                    ui.label("API key");
                    ui.add(
                        TextEdit::singleline(&mut self.draft.api_key)
                            .password(true)
                            .desired_width(420.0),
                    );
                    let needs_api_key =
                        self.draft.provider_kind.requires_api_key() && self.draft.api_key.trim().is_empty();
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
                });

                ui.horizontal(|ui| {
                    ui.label("Base URL");
                    ui.add(TextEdit::singleline(&mut self.draft.base_url).desired_width(420.0));
                });
            });

            ui.add_space(8.0);
            ui.group(|ui| {
                ui.heading("Workspace And Launch");
                ui.horizontal(|ui| {
                    ui.label("Workspace");
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
                            self.draft.workspace = folder;
                        }
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Permission");
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
                    ui.checkbox(&mut self.draft.keep_open, "Keep terminal open");
                });

                ui.label("Prompt");
                ui.add(
                    TextEdit::multiline(&mut self.draft.prompt)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3),
                );

                ui.label("Extra args (one per line)");
                ui.add(
                    TextEdit::multiline(&mut self.args_text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(2),
                );
            });

            ui.add_space(8.0);
            ui.group(|ui| {
                ui.heading("Model And Tools");
                ui.horizontal(|ui| {
                    ui.label("Model");
                    let model_response =
                        ui.add(TextEdit::singleline(&mut self.draft.model).desired_width(280.0));
                    if ui.button("Search").clicked() {
                        self.apply_model_search();
                    }
                    let filtered_models = self.filtered_models();
                    let mut newly_selected_model = None;
                    egui::ComboBox::from_id_salt("model-select")
                        .selected_text(if self.model_search_filter.trim().is_empty() {
                            "Choose known model".to_string()
                        } else {
                            format!("Choose known model ({})", filtered_models.len())
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
                    if let Some(model_id) = newly_selected_model {
                        self.draft.model = model_id;
                        self.model_search_filter = self.draft.model.clone();
                        self.sanitize_selected_tools();
                    }
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
    Arc::new(
        image::load_from_memory(include_bytes!("../../../../../assets/openclaw.ico"))
            .map(|image| {
                let rgba = image.into_rgba8();
                let (width, height) = rgba.dimensions();
                IconData {
                    rgba: rgba.into_raw(),
                    width,
                    height,
                }
            })
            .unwrap_or_default(),
    )
}

fn default_workspace() -> PathBuf {
    PathBuf::from(r"C:\Users\Dean\source\repos\Premiere Project builder")
}

fn provider_presets() -> [ProviderPreset; 7] {
    [
        ProviderPreset {
            kind: ProviderKind::Groq,
            name: "Groq",
            base_url: "https://api.groq.com/openai/v1",
            model: "llama-3.3-70b-versatile",
        },
        ProviderPreset {
            kind: ProviderKind::OpenAi,
            name: "OpenAI",
            base_url: "https://api.openai.com/v1",
            model: "gpt-4.1-mini",
        },
        ProviderPreset {
            kind: ProviderKind::OpenRouter,
            name: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            model: "openai/gpt-oss-120b",
        },
        ProviderPreset {
            kind: ProviderKind::DashScope,
            name: "DashScope",
            base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            model: "qwen/qwen3-32b",
        },
        ProviderPreset {
            kind: ProviderKind::Xai,
            name: "xAI",
            base_url: "https://api.x.ai/v1",
            model: "grok-3-mini",
        },
        ProviderPreset {
            kind: ProviderKind::Anthropic,
            name: "Anthropic",
            base_url: "https://api.anthropic.com",
            model: "claude-sonnet-4-5",
        },
        ProviderPreset {
            kind: ProviderKind::Ollama,
            name: "Ollama",
            base_url: "http://localhost:11434/v1",
            model: "openai/gpt-oss-20b",
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
    if let Some(state) = read_registry_state() {
        return (
            state,
            "Loaded provider profiles from the registry.".to_string(),
        );
    }
    if let Some(legacy) = load_legacy_config(legacy_config_path) {
        let profile = ProviderProfile {
            friendly_name: "Imported provider".to_string(),
            provider_kind: ProviderKind::Custom,
            api_key: legacy.openai_api_key,
            base_url: legacy.openai_base_url,
            workspace: legacy.workspace,
            model: legacy.model,
            permission_mode: legacy.permission_mode,
            allowed_tools: legacy.allowed_tools,
            keep_open: legacy.keep_open,
            prompt: legacy.prompt.unwrap_or_default(),
            args: legacy.args,
        };
        return (
            LauncherState {
                profiles: vec![profile],
                last_selected: Some("Imported provider".to_string()),
            },
            "Imported the legacy launcher config. Save once to migrate it into the registry."
                .to_string(),
        );
    }
    (
        LauncherState {
            profiles: starter_profiles(),
            last_selected: Some("Groq".to_string()),
        },
        "Created starter provider profiles. Add your API key and workspace, then save.".to_string(),
    )
}

fn read_registry_state() -> Option<LauncherState> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey(REGISTRY_PATH).ok()?;
    let body: String = key.get_value(REGISTRY_STATE_VALUE).ok()?;
    serde_json::from_str(&body).ok()
}

fn save_launcher_state(state: &LauncherState) -> Result<(), String> {
    let body = serde_json::to_string_pretty(state)
        .map_err(|error| format!("failed to serialize launcher state: {error}"))?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(REGISTRY_PATH)
        .map_err(|error| format!("failed to open HKCU\\{}: {error}", REGISTRY_PATH))?;
    key.set_value(REGISTRY_STATE_VALUE, &body)
        .map_err(|error| format!("failed to write launcher state to the registry: {error}"))
}

fn write_launch_profile(profile: &ProviderProfile, token_limit: (u32, u32)) -> Result<(), String> {
    let launch_profile = LaunchProfileFile {
        provider_name: profile.friendly_name.clone(),
        provider_kind: profile.provider_kind,
        model: profile.model.clone(),
        base_url: profile.base_url.clone(),
        workspace: profile.workspace.clone(),
        context_window_tokens: token_limit.0,
        max_output_tokens: token_limit.1,
    };
    let body = serde_json::to_string_pretty(&launch_profile)
        .map_err(|error| format!("failed to serialize launch profile: {error}"))?;
    let path = profile.workspace.join(LAUNCH_PROFILE_FILE_NAME);
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
    let mut envs = vec![
        ("CLAW_MODEL", profile.model.clone()),
        ("CLAW_PROVIDER_NAME", profile.friendly_name.clone()),
    ];
    match profile.provider_kind {
        ProviderKind::Anthropic => {
            envs.push(("ANTHROPIC_API_KEY", profile.api_key.clone()));
            envs.push(("ANTHROPIC_BASE_URL", profile.base_url.clone()));
        }
        _ => {
            envs.push(("OPENAI_API_KEY", profile.api_key.clone()));
            envs.push(("OPENAI_BASE_URL", profile.base_url.clone()));
        }
    }
    envs
}

fn provider_default_token_limit(provider_kind: ProviderKind) -> (u32, u32) {
    match provider_kind {
        ProviderKind::Groq => (131_072, 8_192),
        ProviderKind::OpenAi => (128_000, 16_384),
        ProviderKind::OpenRouter => (131_072, 16_384),
        ProviderKind::DashScope => (131_072, 16_384),
        ProviderKind::Xai => (131_072, 16_384),
        ProviderKind::Anthropic => (200_000, 64_000),
        ProviderKind::Ollama => (131_072, 16_384),
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

fn initial_models(provider_kind: ProviderKind, base_url: &str, api_key: &str) -> Vec<ModelView> {
    let mut by_id = known_models()
        .into_iter()
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
        .collect::<BTreeMap<_, _>>();

    if provider_kind.supports_remote_models() {
        if let Ok(remote_models) = fetch_models(provider_kind, base_url, api_key) {
            for remote_model in remote_models {
                let entry = by_id
                    .entry(remote_model.id.clone())
                    .or_insert_with(|| ModelView {
                        id: remote_model.id.clone(),
                        label: remote_model.id.clone(),
                        context_window: 131_072,
                        max_output_tokens: 8_192,
                        tool_use_supported: true,
                        from_api: true,
                    });
                entry.from_api = true;
            }
        }
    }

    let mut models = by_id.into_values().collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
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

fn model_matches_provider(model_id: &str, provider_kind: ProviderKind) -> bool {
    match provider_kind {
        ProviderKind::Groq => matches!(
            model_id,
            "llama-3.3-70b-versatile"
                | "meta-llama/llama-4-scout-17b-16e-instruct"
                | "groq/compound"
                | "qwen/qwen3-32b"
        ),
        ProviderKind::OpenAi => matches!(model_id, "gpt-4.1-mini" | "gpt-4.1"),
        ProviderKind::OpenRouter => true,
        ProviderKind::DashScope => matches!(model_id, "qwen/qwen3-32b"),
        ProviderKind::Xai => matches!(model_id, "grok-3-mini"),
        ProviderKind::Anthropic => matches!(model_id, "claude-sonnet-4-5"),
        ProviderKind::Ollama => matches!(model_id, "openai/gpt-oss-20b" | "openai/gpt-oss-120b"),
        ProviderKind::Custom => true,
    }
}

fn powershell_escape(value: &str) -> String {
    value.replace('\'', "''")
}
