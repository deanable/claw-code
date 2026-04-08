#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eframe::egui::{self, Color32, RichText, TextEdit};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

const VC_REDIST_URL: &str =
    "https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170";
const DEFAULT_BASE_URL: &str = "https://api.groq.com/openai/v1";
const DEFAULT_MODEL: &str = "llama-3.3-70b-versatile";
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const SYSTEM_PROMPT_ESTIMATE: u32 = 2_500;
const BASE_REQUEST_OVERHEAD: u32 = 1_500;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherConfig {
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

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from(r"C:\Users\Dean\source\repos\Premiere Project builder"),
            model: DEFAULT_MODEL.to_string(),
            permission_mode: "danger-full-access".to_string(),
            allowed_tools: vec!["read".to_string(), "glob".to_string(), "grep".to_string()],
            keep_open: true,
            prompt: None,
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            args: Vec::new(),
        }
    }
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
struct GroqModelList {
    data: Vec<GroqModel>,
}

#[derive(Debug, Deserialize)]
struct GroqModel {
    id: String,
}

struct LauncherApp {
    exe_dir: PathBuf,
    claw_path: PathBuf,
    config_path: PathBuf,
    config: LauncherConfig,
    selected_tools: BTreeSet<String>,
    models: Vec<ModelView>,
    status: String,
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Claw Launcher",
        options,
        Box::new(|_cc| Ok(Box::new(LauncherApp::new()))),
    )
}

impl LauncherApp {
    fn new() -> Self {
        let exe_dir = current_exe_dir().unwrap_or_else(|_| PathBuf::from("."));
        let config_path = exe_dir.join("claw-launcher.json");
        let claw_path = exe_dir.join("claw.exe");
        let config = load_config(&config_path).unwrap_or_default();
        let selected_tools = config.allowed_tools.iter().cloned().collect();
        let models = initial_models(&config.openai_api_key);
        let status = if claw_path.is_file() {
            "Ready.".to_string()
        } else {
            format!("Missing {} next to the launcher.", claw_path.display())
        };
        Self {
            exe_dir,
            claw_path,
            config_path,
            config,
            selected_tools,
            models,
            status,
        }
    }

    fn sync_allowed_tools(&mut self) {
        self.config.allowed_tools = self.selected_tools.iter().cloned().collect();
    }

    fn selected_model(&self) -> Option<&ModelView> {
        self.models.iter().find(|model| model.id == self.config.model)
    }

    fn refresh_models(&mut self) {
        self.models = initial_models(&self.config.openai_api_key);
        if self.models.iter().all(|model| model.id != self.config.model) {
            if let Some(first) = self.models.first() {
                self.config.model = first.id.clone();
            }
        }
        self.status = "Model list refreshed.".to_string();
    }

    fn save_config(&mut self) -> Result<(), String> {
        self.sync_allowed_tools();
        if self.config.workspace.as_os_str().is_empty() {
            return Err("Choose a workspace folder first.".to_string());
        }
        if self.config.openai_api_key.trim().is_empty() {
            return Err("Enter a Groq API key first.".to_string());
        }
        let body = serde_json::to_string_pretty(&self.config)
            .map_err(|error| format!("failed to serialize config: {error}"))?;
        fs::write(&self.config_path, body)
            .map_err(|error| format!("failed to write {}: {error}", self.config_path.display()))?;
        set_user_env_var("OPENAI_API_KEY", &self.config.openai_api_key)?;
        set_user_env_var("OPENAI_BASE_URL", &self.config.openai_base_url)?;
        set_user_env_var("CLAW_MODEL", &self.config.model)?;
        self.status = format!("Saved {}", self.config_path.display());
        Ok(())
    }

    fn launch(&mut self, ctx: &egui::Context) -> Result<(), String> {
        self.sync_allowed_tools();
        ensure_runtime_available()?;
        if !self.claw_path.is_file() {
            return Err(format!("Missing {}", self.claw_path.display()));
        }
        if !self.config.workspace.is_dir() {
            return Err(format!(
                "Workspace does not exist: {}",
                self.config.workspace.display()
            ));
        }
        let user_profile = std::env::var("USERPROFILE")
            .map_err(|_| "USERPROFILE is not set on this machine.".to_string())?;

        let mut claw_command = format!(
            "& '{}' --model '{}' --permission-mode '{}'",
            powershell_escape(&self.claw_path.display().to_string()),
            powershell_escape(&self.config.model),
            powershell_escape(&self.config.permission_mode)
        );
        if !self.config.allowed_tools.is_empty() {
            claw_command.push_str(&format!(
                " --allowedTools '{}'",
                powershell_escape(&self.config.allowed_tools.join(","))
            ));
        }
        if let Some(prompt) = self
            .config
            .prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            claw_command.push_str(&format!(" prompt '{}'", powershell_escape(prompt)));
        }
        for arg in &self.config.args {
            claw_command.push_str(&format!(" '{}'", powershell_escape(arg)));
        }

        let mut command = Command::new("powershell");
        command.current_dir(&self.config.workspace);
        command.arg("-NoLogo");
        if self.config.keep_open {
            command.arg("-NoExit");
        }
        command.arg("-Command").arg(claw_command);
        command.env("HOME", user_profile);
        command.env("OPENAI_API_KEY", &self.config.openai_api_key);
        command.env("OPENAI_BASE_URL", &self.config.openai_base_url);
        command.env("CLAW_MODEL", &self.config.model);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NEW_CONSOLE);
        }
        command
            .spawn()
            .map_err(|error| format!("failed to start {}: {error}", self.claw_path.display()))?;
        self.status = "Launching Claw...".to_string();
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
            ui.label("Choose a model, workspace, and tool set, then save and launch.");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("API key");
                ui.add(
                    TextEdit::singleline(&mut self.config.openai_api_key)
                        .password(true)
                        .desired_width(420.0),
                );
                if ui.button("Refresh Models").clicked() {
                    self.refresh_models();
                }
            });

            ui.horizontal(|ui| {
                ui.label("Base URL");
                ui.text_edit_singleline(&mut self.config.openai_base_url);
            });

            ui.horizontal(|ui| {
                ui.label("Workspace");
                let mut workspace_display = self.config.workspace.display().to_string();
                ui.add_enabled(
                    false,
                    TextEdit::singleline(&mut workspace_display).desired_width(420.0),
                );
                if ui.button("Select Folder").clicked() {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_directory(&self.config.workspace)
                        .pick_folder()
                    {
                        self.config.workspace = folder;
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label("Model");
                egui::ComboBox::from_id_salt("model-select")
                    .selected_text(self.config.model.clone())
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        for model in &self.models {
                            let text = format!(
                                "{}  [{} ctx / {} out{}]",
                                model.id,
                                model.context_window,
                                model.max_output_tokens,
                                if model.tool_use_supported { ", tool use" } else { ", no tool use" }
                            );
                            ui.selectable_value(&mut self.config.model, model.id.clone(), text);
                        }
                    });
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
                    if model.from_api { " | listed by Groq API" } else { " | bundled metadata" }
                ));
            }

            ui.separator();
            ui.label("Available Tools");
            for tool in available_tools() {
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

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save Config").clicked() {
                    match self.save_config() {
                        Ok(()) => {}
                        Err(error) => self.status = error,
                    }
                }
                if ui.button("Launch").clicked() {
                    match self.save_config().and_then(|_| self.launch(ctx)) {
                        Ok(()) => {}
                        Err(error) => self.status = error,
                    }
                }
            });
            ui.checkbox(
                &mut self.config.keep_open,
                "Keep terminal open after Claw exits",
            );

            ui.separator();
            ui.label(&self.status);
            ui.small(format!(
                "Config file: {}",
                self.config_path.display()
            ));
            ui.small(format!("Launcher directory: {}", self.exe_dir.display()));
        });
    }
}

fn load_config(path: &Path) -> Option<LauncherConfig> {
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

fn set_user_env_var(key: &str, value: &str) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (env_key, _) = hkcu
        .create_subkey("Environment")
        .map_err(|error| format!("failed to open HKCU\\Environment: {error}"))?;
    env_key
        .set_value(key, &value)
        .map_err(|error| format!("failed to persist user environment variable {key}: {error}"))
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
    ]
}

fn initial_models(api_key: &str) -> Vec<ModelView> {
    let mut known = known_models()
        .into_iter()
        .map(|model| ModelView {
            id: model.id.to_string(),
            label: model.label.to_string(),
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            tool_use_supported: model.tool_use_supported,
            from_api: false,
        })
        .collect::<Vec<_>>();

    if api_key.trim().is_empty() {
        known.sort_by(|a, b| a.id.cmp(&b.id));
        return known;
    }

    let by_id = known
        .iter()
        .map(|model| (model.id.clone(), model.clone()))
        .collect::<BTreeMap<_, _>>();

    match fetch_groq_models(api_key) {
        Ok(models) => {
            let mut merged = Vec::new();
            for groq_model in models {
                if let Some(known_model) = by_id.get(&groq_model.id) {
                    let mut model = known_model.clone();
                    model.from_api = true;
                    merged.push(model);
                }
            }
            if merged.is_empty() {
                known.sort_by(|a, b| a.id.cmp(&b.id));
                return known;
            }
            merged.sort_by(|a, b| a.id.cmp(&b.id));
            merged
        }
        Err(_) => {
            known.sort_by(|a, b| a.id.cmp(&b.id));
            known
        }
    }
}

fn fetch_groq_models(api_key: &str) -> Result<Vec<GroqModel>, String> {
    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;
    let response = client
        .get(format!("{DEFAULT_BASE_URL}/models"))
        .bearer_auth(api_key)
        .send()
        .map_err(|error| format!("failed to fetch models from Groq: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Groq models request failed with {}", response.status()));
    }
    response
        .json::<GroqModelList>()
        .map(|payload| payload.data)
        .map_err(|error| format!("failed to parse Groq models response: {error}"))
}

fn powershell_escape(value: &str) -> String {
    value.replace('\'', "''")
}
