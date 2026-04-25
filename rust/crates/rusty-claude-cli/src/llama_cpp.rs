use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;

const DEFAULT_LLAMA_CPP_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const LLAMA_CPP_MODEL_PREFIX: &str = "llama.cpp/";
const LLAMA_CPP_BIN_DIR_NAME: &str = "llama-b8857-bin-win-cpu-x64";
const DEFAULT_SEARCH_LIMIT: usize = 20;

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

fn llama_cpp_base_url() -> String {
    std::env::var("LLAMA_CPP_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_LLAMA_CPP_BASE_URL.to_string())
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
        .timeout(Duration::from_secs(2))
        .send()
        .is_ok_and(|resp| resp.status().is_success())
}

fn parse_host_port(base_url: &str) -> Result<(String, u16), String> {
    let trimmed = base_url.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
        "LLAMA_CPP_BASE_URL must include a scheme (example: http://127.0.0.1:8080/v1)".to_string()
    })?;
    let authority = rest.split('/').next().unwrap_or("").trim();
    if authority.is_empty() {
        return Err("LLAMA_CPP_BASE_URL is missing a host".to_string());
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
            .map_err(|_| format!("invalid port in LLAMA_CPP_BASE_URL: '{last}'"))?;
        Ok((host.to_string(), port))
    } else {
        Ok((last.to_string(), default_port))
    }
}

fn ensure_model_downloaded(
    repo_root: &Path,
    repo_id: &str,
    filename: &str,
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
    let mut request = client.get(&url).timeout(Duration::from_secs(120));
    if let Some(token) = huggingface_token() {
        request = request.bearer_auth(token);
    }
    let mut response = request
        .send()
        .map_err(|error| format!("failed to download GGUF from Hugging Face: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "download failed: GET {url} -> {}",
            response.status()
        ));
    }

    let part_path = dest_path.with_extension("gguf.part");
    let mut output =
        fs::File::create(&part_path).map_err(|error| format!("failed to write model: {error}"))?;
    std::io::copy(&mut response, &mut output)
        .map_err(|error| format!("failed to write GGUF to disk: {error}"))?;
    output
        .flush()
        .map_err(|error| format!("failed to flush GGUF to disk: {error}"))?;
    fs::rename(&part_path, &dest_path)
        .map_err(|error| format!("failed to finalize GGUF download: {error}"))?;

    Ok(dest_path)
}

fn start_llama_cpp_server(
    client: &Client,
    base_url: &str,
    bin_dir: &Path,
    model_path: &Path,
) -> Result<(), String> {
    if llama_cpp_is_ready(client, base_url) {
        return Ok(());
    }

    let server = bin_dir.join("llama-server.exe");
    if !server.is_file() {
        return Err(format!(
            "llama.cpp server binary not found at {}. Set LLAMA_CPP_BIN_DIR or place {} in your repo root.",
            server.display(),
            LLAMA_CPP_BIN_DIR_NAME
        ));
    }

    let (host, port) = parse_host_port(base_url)?;

    let mut command = Command::new(&server);
    command.current_dir(bin_dir);
    command.arg("-m").arg(model_path);
    command.arg("--host").arg(host);
    command.arg("--port").arg(port.to_string());

    #[cfg(target_os = "windows")]
    {
        // Avoid a second console window popping up when the server starts.
        // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
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
            server.display()
        )
    })?;

    let url = base_url_models_endpoint(base_url);
    for _ in 0..40 {
        if llama_cpp_is_ready(client, base_url) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    Err(format!(
        "llama.cpp server did not become ready at {url} after waiting."
    ))
}

fn resolve_openai_model_id(client: &Client, base_url: &str) -> Result<String, String> {
    let url = base_url_models_endpoint(base_url);
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(5))
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

pub fn search(query: Option<&str>) -> Result<Vec<String>, String> {
    let q = query.unwrap_or("gguf").trim();
    let q = if q.is_empty() { "gguf" } else { q };
    let limit = DEFAULT_SEARCH_LIMIT.to_string();

    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;

    let mut request = client.get("https://huggingface.co/api/models").query(&[
        ("search", q),
        ("limit", limit.as_str()),
        ("full", "true"),
        ("sort", "downloads"),
        ("direction", "-1"),
    ]);
    if let Some(token) = huggingface_token() {
        request = request.bearer_auth(token);
    }
    let response = request
        .timeout(Duration::from_secs(10))
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
        .map_err(|error| format!("failed to parse Hugging Face model search response: {error}"))?;

    let mut specs = Vec::new();
    for model in results {
        for sibling in model.siblings {
            if !sibling.filename.to_ascii_lowercase().ends_with(".gguf") {
                continue;
            }
            specs.push(format!(
                "{}{}::{}",
                LLAMA_CPP_MODEL_PREFIX, model.id, sibling.filename
            ));
            if specs.len() >= 120 {
                break;
            }
        }
        if specs.len() >= 120 {
            break;
        }
    }
    if specs.is_empty() {
        return Err(format!("no GGUF files found for '{q}'"));
    }
    specs.sort();
    Ok(specs)
}

pub fn download(spec: &str) -> Result<PathBuf, String> {
    let (repo_id, filename) = parse_llama_cpp_spec(spec)
        .or_else(|| parse_bare_spec(spec))
        .ok_or_else(|| {
            "expected spec 'llama.cpp/<repo_id>::<filename.gguf>' (or '<repo_id>::<filename.gguf>')"
                .to_string()
        })?;
    if !filename.to_ascii_lowercase().ends_with(".gguf") {
        return Err(format!(
            "llama.cpp model spec must reference a .gguf file (got '{filename}')."
        ));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let bin_dir = find_llama_cpp_bin_dir(&cwd).ok_or_else(|| {
        format!(
            "could not find {LLAMA_CPP_BIN_DIR_NAME}. Set LLAMA_CPP_BIN_DIR or place the directory at your repo root."
        )
    })?;
    let repo_root = bin_dir
        .parent()
        .map_or_else(|| cwd.clone(), Path::to_path_buf);
    ensure_model_downloaded(&repo_root, &repo_id, &filename)
}

pub fn maybe_bootstrap(model: &str) -> Result<Option<String>, String> {
    let Some((repo_id, filename)) = parse_llama_cpp_spec(model) else {
        return Ok(None);
    };

    if !filename.to_ascii_lowercase().ends_with(".gguf") {
        return Err(format!(
            "llama.cpp model spec must reference a .gguf file (got '{filename}')."
        ));
    }

    let base_url = llama_cpp_base_url();
    std::env::set_var("OPENAI_BASE_URL", &base_url);

    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let bin_dir = find_llama_cpp_bin_dir(&cwd).ok_or_else(|| {
        format!(
            "could not find {LLAMA_CPP_BIN_DIR_NAME}. Set LLAMA_CPP_BIN_DIR or place the directory at your repo root."
        )
    })?;
    let repo_root = bin_dir
        .parent()
        .map_or_else(|| cwd.clone(), Path::to_path_buf);

    let model_path = ensure_model_downloaded(&repo_root, &repo_id, &filename)?;

    let client = Client::builder()
        .build()
        .map_err(|error| format!("http client build failed: {error}"))?;
    start_llama_cpp_server(&client, &base_url, &bin_dir, &model_path)?;

    resolve_openai_model_id(&client, &base_url)
        .map(Some)
        .or_else(|_| Ok(Some("llama.cpp".to_string())))
}
