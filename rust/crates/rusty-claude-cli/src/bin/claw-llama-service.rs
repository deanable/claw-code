#![cfg(target_os = "windows")]

use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

const LLAMA_CPP_SERVICE_NAME: &str = "ClawLlamaCpp";
const SERVICE_CONFIG_FILENAME: &str = "claw-llama-service.json";
const SERVICE_LOG_FILENAME: &str = "claw-llama-service.log";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceConfig {
    server_exe: PathBuf,
    model_path: PathBuf,
    host: String,
    port: u16,
}

fn exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("failed to resolve parent directory for {}", exe.display()))
}

fn log_service_event(message: impl AsRef<str>) {
    let Ok(dir) = exe_dir() else {
        return;
    };
    let path = dir.join(SERVICE_LOG_FILENAME);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |duration| duration.as_secs());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{timestamp}] {}", message.as_ref());
    }
}

fn load_config() -> Result<ServiceConfig, String> {
    let dir = exe_dir()?;
    let path = dir.join(SERVICE_CONFIG_FILENAME);
    let bytes =
        fs::read(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let bytes = trim_ascii_whitespace(bytes);
    serde_json::from_slice::<ServiceConfig>(bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if !first.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    while let Some((last, rest)) = bytes.split_last() {
        if !last.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    bytes
}

fn spawn_server(config: &ServiceConfig) -> Result<Child, String> {
    if !config.server_exe.is_file() {
        return Err(format!(
            "server exe not found at {}",
            config.server_exe.display()
        ));
    }
    if !config.model_path.is_file() {
        return Err(format!(
            "model not found at {}",
            config.model_path.display()
        ));
    }
    let bin_dir = config
        .server_exe
        .parent()
        .ok_or_else(|| format!("invalid server exe path: {}", config.server_exe.display()))?;

    let mut command = Command::new(&config.server_exe);
    command.current_dir(bin_dir);
    command.arg("-m").arg(&config.model_path);
    command.arg("--host").arg(&config.host);
    command.arg("--port").arg(config.port.to_string());

    // Ensure the server can locate its dependent DLLs.
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    command.env(
        "PATH",
        format!("{};{}", bin_dir.display(), existing_path.to_string_lossy()),
    );

    #[cfg(target_os = "windows")]
    {
        // Avoid a console window when running as a service.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command
        .spawn()
        .map_err(|error| format!("failed to start llama-server.exe: {error}"))
}

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // For debugging the service wrapper as a normal console app:
    // `claw-llama-service.exe --run-once`
    if std::env::args().any(|arg| arg == "--run-once") {
        log_service_event("run_once");
        let config = load_config().map_err(std::io::Error::other)?;
        let mut child = spawn_server(&config).map_err(std::io::Error::other)?;
        let status = child.wait().map_err(std::io::Error::other)?;
        log_service_event(format!("run_once_exit status={status}"));
        return Ok(());
    }

    service_dispatcher::start(LLAMA_CPP_SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        log_service_event(format!("fatal error={error}"));
    }
}

fn run_service() -> Result<(), String> {
    log_service_event("service_start");

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let stop_tx_handle = stop_tx.clone();

    let status_handle =
        service_control_handler::register(LLAMA_CPP_SERVICE_NAME, move |event| match event {
            ServiceControl::Stop => {
                let _ = stop_tx_handle.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .map_err(|error| format!("failed to register service control handler: {error}"))?;

    status_handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(15),
            process_id: None,
        })
        .map_err(|error| format!("failed to set service start-pending status: {error}"))?;

    let config = load_config().inspect_err(|_error| {
        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(2),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });
    })?;
    log_service_event(format!(
        "config server='{}' model='{}' host='{}' port={}",
        config.server_exe.display(),
        config.model_path.display(),
        config.host,
        config.port
    ));

    status_handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .map_err(|error| format!("failed to set service running status: {error}"))?;

    let mut child = spawn_server(&config).inspect_err(|_error| {
        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(3),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });
    })?;

    // Block until we receive a stop request or the server exits.
    loop {
        if stop_rx.try_recv().is_ok() {
            log_service_event("service_stop_requested");
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                log_service_event(format!("server_exit status={status}"));
                break;
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => {
                log_service_event(format!("server_wait_error error={error}"));
                break;
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    status_handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .map_err(|error| format!("failed to set service stopped status: {error}"))?;

    log_service_event("service_stopped");
    Ok(())
}
