//! rag-engine integration surface.
//!
//! The generated protobuf bindings are compiled from the git submodule at
//! `external/rag-engine/engine/proto/engine.proto`. Thuki treats the Go control
//! plane as the desktop boundary and talks to Runtime, Rag, and Context over
//! gRPC. HTTP remains a diagnostics/readiness surface owned by rag-engine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Manager, State};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel as TonicChannel;

use crate::config::defaults::DEFAULT_ENGINE_MODE;
use crate::config::AppConfig;

pub mod pb {
    tonic::include_proto!("engine");
}

const ENGINE_BINARY_BASENAME: &str = "ai-engine-server";
const ENGINE_CONFIG_FILE_NAME: &str = "config.yaml";
const ENGINE_DATA_DIR_NAME: &str = "rag-engine";
const DEFAULT_ENGINE_DAEMON_PORT: u16 = 50061;
const DEFAULT_CONTEXT_SERVICE_PORT: u16 = 9191;
const LOCAL_CONTEXT_MAX_SNIPPET_CHARS: usize = 900;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("failed to connect to rag-engine: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("rag-engine gRPC error: {0}")]
    Status(#[from] tonic::Status),
    #[error("rag-engine sidecar binary was not found")]
    MissingBinary,
    #[error("rag-engine startup timed out")]
    StartupTimeout,
    #[error("engine I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to resolve app path: {0}")]
    Path(String),
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourcePreview {
    pub title: String,
    pub uri: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EngineModelPreview {
    pub id: String,
    pub name: String,
    pub loaded: bool,
    pub vision: bool,
    pub thinking: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EngineRuntimeStatus {
    pub reachable: bool,
    pub healthy: bool,
    pub version: String,
    pub loaded_models: Vec<EngineModelPreview>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EngineRagStatus {
    pub document_count: i64,
    pub chunk_count: i64,
    pub embedding_model: String,
    pub embedding_provider: String,
    pub requires_reindex: bool,
    pub reindex_reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineStreamEvent {
    Token(String),
    Done,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInferenceParams {
    pub grpc_url: String,
    pub model_id: String,
    pub prompt: String,
    pub system_prompt: Option<String>,
}

#[derive(Default, Clone)]
pub struct EngineClient;

impl EngineClient {
    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn runtime_client(
        &self,
        grpc_url: &str,
    ) -> Result<pb::runtime_client::RuntimeClient<TonicChannel>, EngineError> {
        Ok(pb::runtime_client::RuntimeClient::connect(grpc_url.to_string()).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn rag_client(
        &self,
        grpc_url: &str,
    ) -> Result<pb::rag_client::RagClient<TonicChannel>, EngineError> {
        Ok(pb::rag_client::RagClient::connect(grpc_url.to_string()).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn context_client(
        &self,
        grpc_url: &str,
    ) -> Result<pb::context_client::ContextClient<TonicChannel>, EngineError> {
        Ok(pb::context_client::ContextClient::connect(grpc_url.to_string()).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn get_status(&self, grpc_url: &str) -> Result<EngineRuntimeStatus, EngineError> {
        let mut client = self.runtime_client(grpc_url).await?;
        let status = client.get_status(()).await?.into_inner();
        Ok(map_runtime_status(status, true))
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn list_models(
        &self,
        grpc_url: &str,
    ) -> Result<Vec<EngineModelPreview>, EngineError> {
        let mut client = self.runtime_client(grpc_url).await?;
        let models = client.list_models(()).await?.into_inner();
        Ok(models.models.iter().map(map_model_info).collect())
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn load_model(
        &self,
        grpc_url: &str,
        model_id: &str,
    ) -> Result<EngineModelPreview, EngineError> {
        let mut client = self.runtime_client(grpc_url).await?;
        let model = client
            .load_model(pb::LoadModelRequest {
                model_id: model_id.to_string(),
                options: HashMap::new(),
            })
            .await?
            .into_inner();
        Ok(map_model_info(&model))
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn unload_model(&self, grpc_url: &str, model_id: &str) -> Result<(), EngineError> {
        let mut client = self.runtime_client(grpc_url).await?;
        client
            .unload_model(pb::UnloadModelRequest {
                model_id: model_id.to_string(),
            })
            .await?;
        Ok(())
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn search_rag(
        &self,
        grpc_url: &str,
        query: &str,
        top_k: u32,
    ) -> Result<Vec<ContextSourcePreview>, EngineError> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let mut client = self.rag_client(grpc_url).await?;
        let response = client
            .search(pb::SearchRequest {
                query: query.to_string(),
                top_k: top_k as i32,
                filters: HashMap::new(),
            })
            .await?
            .into_inner();
        Ok(response.results.into_iter().map(map_rag_result).collect())
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn get_rag_status(&self, grpc_url: &str) -> Result<EngineRagStatus, EngineError> {
        let mut client = self.rag_client(grpc_url).await?;
        let status = client.get_rag_status(()).await?.into_inner();
        Ok(EngineRagStatus {
            document_count: status.document_count,
            chunk_count: status.chunk_count,
            embedding_model: status.embedding_model,
            embedding_provider: status.embedding_provider,
            requires_reindex: status.requires_reindex,
            reindex_reasons: status.reindex_reasons,
        })
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn append_session_turns(
        &self,
        grpc_url: &str,
        session_id: &str,
        user_content: &str,
        assistant_content: &str,
    ) -> Result<(), EngineError> {
        let mut client = self.context_client(grpc_url).await?;
        client
            .append_session(pb::ContextSessionAppendRequest {
                session_id: session_id.to_string(),
                role: "user".to_string(),
                content: user_content.to_string(),
                metadata: HashMap::new(),
            })
            .await?;
        client
            .append_session(pb::ContextSessionAppendRequest {
                session_id: session_id.to_string(),
                role: "assistant".to_string(),
                content: assistant_content.to_string(),
                metadata: HashMap::new(),
            })
            .await?;
        Ok(())
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn stream_inference(
        &self,
        params: EngineInferenceParams,
        cancel_token: CancellationToken,
        mut on_event: impl FnMut(EngineStreamEvent),
    ) -> Result<String, EngineError> {
        let mut client = self.runtime_client(&params.grpc_url).await?;
        let request = pb::InferenceRequest {
            model_id: params.model_id,
            prompt: params.prompt,
            parameters: default_runtime_parameters(),
            provider: String::new(),
            context_refs: Vec::new(),
            system_prompt: params.system_prompt,
        };
        let outbound = tokio_stream::iter(vec![request]);
        let mut stream = client
            .stream_inference(tonic::Request::new(outbound))
            .await?
            .into_inner();
        let mut accumulated = String::new();

        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    on_event(EngineStreamEvent::Cancelled);
                    return Ok(accumulated);
                }
                maybe = stream.next() => {
                    let Some(message) = maybe else {
                        return Ok(accumulated);
                    };
                    let response = message?;
                    if !response.token.is_empty() {
                        accumulated.push_str(&response.token);
                        on_event(EngineStreamEvent::Token(response.token));
                    }
                    if response.complete {
                        on_event(EngineStreamEvent::Done);
                        return Ok(accumulated);
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub struct EngineSupervisor {
    child: Mutex<Option<Child>>,
}

impl EngineSupervisor {
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn is_running(&self) -> bool {
        let mut guard = self.child.lock();
        let Some(child) = guard.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => {
                *guard = None;
                false
            }
        }
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn start_managed(
        &self,
        app: &tauri::AppHandle,
        config: &AppConfig,
        app_data_dir: &Path,
    ) -> Result<(), EngineError> {
        if self.is_running() {
            return Ok(());
        }

        let binary = resolve_engine_binary(app)?.ok_or(EngineError::MissingBinary)?;
        let engine_dir = engine_root_dir(app_data_dir);
        std::fs::create_dir_all(&engine_dir)?;
        let config_path = engine_dir.join(ENGINE_CONFIG_FILE_NAME);
        std::fs::write(&config_path, render_engine_config(app_data_dir, config))?;

        let child = Command::new(&binary)
            .arg("-config")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(binary.parent().unwrap_or_else(|| Path::new(".")))
            .spawn()?;

        *self.child.lock() = Some(child);
        Ok(())
    }
}

impl Drop for EngineSupervisor {
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub fn spawn_managed_engine(app: tauri::AppHandle, config: AppConfig) {
    if !should_start_managed_engine(&config) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        if let Err(err) = start_managed_engine_for_app(app, config).await {
            eprintln!("thuki: [rag-engine] managed startup failed: {err}");
        }
    });
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn start_managed_engine_for_app(
    app: tauri::AppHandle,
    config: AppConfig,
) -> Result<(), EngineError> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| EngineError::Path(err.to_string()))?;
    {
        let supervisor = app.state::<EngineSupervisor>();
        supervisor.start_managed(&app, &config, &app_data_dir)?;
    }
    wait_for_grpc_ready(
        &EngineClient,
        &config.engine.grpc_url,
        Duration::from_secs(config.engine.startup_timeout_s),
    )
    .await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn wait_for_grpc_ready(
    client: &EngineClient,
    grpc_url: &str,
    timeout: Duration,
) -> Result<(), EngineError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if client.get_status(grpc_url).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(EngineError::StartupTimeout);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg_attr(not(coverage), tauri::command)]
pub async fn get_engine_status(
    client: State<'_, EngineClient>,
    config: State<'_, parking_lot::RwLock<AppConfig>>,
) -> Result<EngineRuntimeStatus, String> {
    let engine_config = config.read().engine.clone();
    if !engine_config.enabled {
        return Ok(EngineRuntimeStatus {
            reachable: false,
            healthy: false,
            version: String::new(),
            loaded_models: Vec::new(),
        });
    }
    client
        .get_status(&engine_config.grpc_url)
        .await
        .map_err(|err| err.to_string())
}

pub fn should_start_managed_engine(config: &AppConfig) -> bool {
    config.engine.enabled && config.engine.mode == DEFAULT_ENGINE_MODE
}

pub fn should_route_to_engine(config: &AppConfig, has_images: bool, think: bool) -> bool {
    config.engine.enabled && !has_images && !think
}

pub fn should_fallback_to_ollama(config: &AppConfig) -> bool {
    config.engine.fallback_to_ollama
}

pub fn build_augmented_prompt(message: &str, sources: &[ContextSourcePreview]) -> String {
    if sources.is_empty() {
        return message.to_string();
    }

    let mut prompt = String::from("Local context:\n");
    for (index, source) in sources.iter().enumerate() {
        let snippet = truncate_chars(source.snippet.trim(), LOCAL_CONTEXT_MAX_SNIPPET_CHARS);
        prompt.push_str(&format!(
            "[{}] {}\nURI: {}\n{}\n\n",
            index + 1,
            source.title.trim(),
            source.uri.trim(),
            snippet
        ));
    }
    prompt.push_str("User request:\n");
    prompt.push_str(message);
    prompt
}

pub fn map_model_info(model: &pb::ModelInfo) -> EngineModelPreview {
    EngineModelPreview {
        id: model.id.clone(),
        name: if model.name.trim().is_empty() {
            model.id.clone()
        } else {
            model.name.clone()
        },
        loaded: model.loaded,
        vision: metadata_bool(&model.metadata, "vision"),
        thinking: metadata_bool(&model.metadata, "thinking"),
    }
}

pub fn map_runtime_status(status: pb::RuntimeStatus, reachable: bool) -> EngineRuntimeStatus {
    EngineRuntimeStatus {
        reachable,
        healthy: status.healthy,
        version: status.version,
        loaded_models: status.loaded_models.iter().map(map_model_info).collect(),
    }
}

pub fn map_rag_result(result: pb::SearchResult) -> ContextSourcePreview {
    let title = first_metadata_value(&result.metadata, &["title", "name", "path"])
        .unwrap_or_else(|| result.document_id.clone());
    let uri = first_metadata_value(&result.metadata, &["uri", "url", "source", "path"])
        .unwrap_or_else(|| result.document_id.clone());
    ContextSourcePreview {
        title,
        uri,
        snippet: result.chunk_text,
    }
}

pub fn default_runtime_parameters() -> HashMap<String, String> {
    HashMap::from([
        ("temperature".to_string(), "1.0".to_string()),
        ("top_p".to_string(), "0.95".to_string()),
        ("top_k".to_string(), "64".to_string()),
    ])
}

pub fn engine_root_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(ENGINE_DATA_DIR_NAME)
}

pub fn render_engine_config(app_data_dir: &Path, config: &AppConfig) -> String {
    let (http_host, http_port) = parse_endpoint(&config.engine.http_url, "127.0.0.1", 8080);
    let (grpc_host, grpc_port) = parse_endpoint(&config.engine.grpc_url, "127.0.0.1", 50051);
    let engine_dir = engine_root_dir(app_data_dir);
    let models_dir = engine_dir.join("models");
    let rag_dir = engine_dir.join("rag");
    let context_dir = engine_dir.join("context");
    let embedding_cache_dir = engine_dir.join("embedding-cache");
    let training_dir = engine_dir.join("training");

    format!(
        r#"server:
  host: "{http_host}"
  port: {http_port}
  mode: "production"
  grpc:
    host: "{grpc_host}"
    port: {grpc_port}
  cors:
    enabled: true
    allowed_origins:
      - "http://localhost:*"
      - "http://127.0.0.1:*"
      - "app://ai-engine"
    allowed_headers:
      - "Content-Type"
      - "Authorization"

daemon:
  host: "127.0.0.1"
  port: {DEFAULT_ENGINE_DAEMON_PORT}
  required: true
  command: ""
  args: []
  startup_timeout: {startup_timeout}s
  restart_backoff: 3s
  ready_timeout: 10s
  llama_cli: "llama-cli"
  training_cli: "llama-train"

services:
  enable_training: false
  enable_mcp: false

storage:
  lancedb_uri: "{rag_dir}"
  enable_fts: true
  enable_hybrid_search: true

runtime:
  models_path: "{models_dir}"
  backend: "mistralrs"
  providers: []
  mistralrs:
    force_cpu: false
    max_num_seqs: 32
    auto_isq: ""
    paged_attn_block_size: 0
    paged_attn_gpu_mem_ctx: 0
    paged_attn_cache_dtype: ""

huggingface:
  enabled: true
  endpoint: "https://huggingface.co"
  max_download_bytes: 0
  compatible_extensions:
    - ".gguf"
    - ".ggml"
    - ".bin"

context:
  enabled: true
  service_url: "http://127.0.0.1:{DEFAULT_CONTEXT_SERVICE_PORT}"
  binary_path: "context_server"
  data_dir: "{context_dir}"
  auto_start: true
  startup_timeout: 20s
  managed_roots:
    - "workspace=."
  openviking:
    url: ""
    api_key: ""

rag:
  storage_path: "{rag_dir}"
  embedding_provider: "fastembed"
  embedding_model: "sentence-transformers/all-MiniLM-L6-v2"
  embedding_cache_dir: "{embedding_cache_dir}"
  embedding_allow_download: true
  chunk_size: 512
  chunk_overlap: 50
  top_k: {context_top_k}

training:
  working_dir: "{training_dir}"
  max_concurrent_jobs: 2

mcp:
  timeout: 30s
  retries: 3

logging:
  level: "info"
  format: "json"
"#,
        http_host = yaml_escape(&http_host),
        grpc_host = yaml_escape(&grpc_host),
        rag_dir = yaml_path(&rag_dir),
        models_dir = yaml_path(&models_dir),
        context_dir = yaml_path(&context_dir),
        embedding_cache_dir = yaml_path(&embedding_cache_dir),
        training_dir = yaml_path(&training_dir),
        startup_timeout = config.engine.startup_timeout_s,
        context_top_k = config.engine.context_top_k,
    )
}

pub fn parse_endpoint(url: &str, fallback_host: &str, fallback_port: u16) -> (String, u16) {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let authority = without_scheme.split('/').next().unwrap_or("");
    let authority = authority.trim();
    if authority.is_empty() {
        return (fallback_host.to_string(), fallback_port);
    }
    let Some((host, port)) = authority.rsplit_once(':') else {
        return (authority.to_string(), fallback_port);
    };
    let parsed_port = port.parse::<u16>().unwrap_or(fallback_port);
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.is_empty() {
        (fallback_host.to_string(), parsed_port)
    } else {
        (host.to_string(), parsed_port)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn resolve_engine_binary(app: &tauri::AppHandle) -> Result<Option<PathBuf>, EngineError> {
    if let Some(path) = std::env::var_os("THUKI_ENGINE_SERVER").map(PathBuf::from) {
        if path.is_file() {
            return Ok(Some(path));
        }
    }

    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.extend(candidate_engine_binary_paths(&resource_dir));
    }
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.extend(candidate_engine_binary_paths(&current_dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.extend(candidate_engine_binary_paths(dir));
        }
    }
    Ok(first_existing_file(candidates))
}

pub fn candidate_engine_binary_paths(root: &Path) -> Vec<PathBuf> {
    let binary = platform_binary_name(ENGINE_BINARY_BASENAME);
    vec![
        root.join(&binary),
        root.join("binaries").join(&binary),
        root.join("src-tauri").join("binaries").join(&binary),
        root.join("external")
            .join("rag-engine")
            .join("engine")
            .join("go")
            .join("bin")
            .join(&binary),
    ]
}

fn platform_binary_name_for(base: &str, is_windows: bool) -> String {
    if is_windows {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

pub fn platform_binary_name(base: &str) -> String {
    platform_binary_name_for(base, cfg!(windows))
}

pub fn first_existing_file(paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| path.is_file())
}

fn metadata_bool(metadata: &HashMap<String, String>, key: &str) -> bool {
    metadata
        .get(key)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes"
            )
        })
        .unwrap_or(false)
}

fn first_metadata_value(metadata: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| metadata.get(*key))
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn yaml_path(path: &Path) -> String {
    yaml_escape(&path.display().to_string().replace('\\', "/"))
}

fn yaml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_config() -> AppConfig {
        AppConfig::default()
    }

    #[test]
    fn route_to_engine_only_for_plain_text_enabled_turns() {
        let config = app_config();
        assert!(should_route_to_engine(&config, false, false));
        assert!(!should_route_to_engine(&config, true, false));
        assert!(!should_route_to_engine(&config, false, true));

        let mut disabled = app_config();
        disabled.engine.enabled = false;
        assert!(!should_route_to_engine(&disabled, false, false));
    }

    #[test]
    fn fallback_to_ollama_follows_engine_config() {
        let mut config = app_config();
        assert!(should_fallback_to_ollama(&config));

        config.engine.fallback_to_ollama = false;
        assert!(!should_fallback_to_ollama(&config));
    }

    #[test]
    fn start_managed_requires_enabled_managed_mode() {
        let mut config = app_config();
        assert!(should_start_managed_engine(&config));
        config.engine.mode = "external".to_string();
        assert!(!should_start_managed_engine(&config));
        config.engine.enabled = false;
        config.engine.mode = DEFAULT_ENGINE_MODE.to_string();
        assert!(!should_start_managed_engine(&config));
    }

    #[test]
    fn parse_endpoint_extracts_host_and_port() {
        assert_eq!(
            parse_endpoint("http://127.0.0.1:50051", "localhost", 1),
            ("127.0.0.1".to_string(), 50051)
        );
        assert_eq!(
            parse_endpoint("http://localhost:8080/health", "fallback", 42),
            ("localhost".to_string(), 8080)
        );
        assert_eq!(
            parse_endpoint("", "fallback", 42),
            ("fallback".to_string(), 42)
        );
        assert_eq!(
            parse_endpoint("127.0.0.1", "fallback", 42),
            ("127.0.0.1".to_string(), 42)
        );
        assert_eq!(
            parse_endpoint("http://:50051", "fallback", 42),
            ("fallback".to_string(), 50051)
        );
    }

    #[test]
    fn render_engine_config_uses_app_data_paths_and_config_ports() {
        let mut config = app_config();
        config.engine.grpc_url = "http://127.0.0.1:50052".to_string();
        config.engine.http_url = "http://127.0.0.1:8081".to_string();
        config.engine.context_top_k = 7;
        let yaml = render_engine_config(Path::new("C:\\Users\\kevin\\AppData\\Thuki"), &config);

        assert!(yaml.contains("port: 8081"));
        assert!(yaml.contains("port: 50052"));
        assert!(yaml.contains("C:/Users/kevin/AppData/Thuki/rag-engine/models"));
        assert!(yaml.contains("C:/Users/kevin/AppData/Thuki/rag-engine/rag"));
        assert!(yaml.contains("C:/Users/kevin/AppData/Thuki/rag-engine/context"));
        assert!(yaml.contains("top_k: 7"));
        assert!(yaml.contains("service_url: \"http://127.0.0.1:9191\""));
    }

    #[test]
    fn map_model_defaults_to_text_only_until_metadata_says_otherwise() {
        let model = pb::ModelInfo {
            id: "qwen".to_string(),
            name: String::new(),
            path: String::new(),
            size_bytes: 0,
            loaded: true,
            metadata: HashMap::new(),
        };
        assert_eq!(
            map_model_info(&model),
            EngineModelPreview {
                id: "qwen".to_string(),
                name: "qwen".to_string(),
                loaded: true,
                vision: false,
                thinking: false,
            }
        );

        let mut with_caps = model;
        with_caps.name = "Qwen".to_string();
        with_caps
            .metadata
            .insert("vision".to_string(), "true".to_string());
        with_caps
            .metadata
            .insert("thinking".to_string(), "1".to_string());
        let mapped = map_model_info(&with_caps);
        assert_eq!(mapped.name, "Qwen");
        assert!(mapped.vision);
        assert!(mapped.thinking);
    }

    #[test]
    fn map_rag_result_prefers_metadata_title_and_uri() {
        let result = pb::SearchResult {
            document_id: "doc-1".to_string(),
            chunk_text: "local snippet".to_string(),
            score: 0.91,
            metadata: HashMap::from([
                ("title".to_string(), "Design Notes".to_string()),
                ("uri".to_string(), "file:///notes.md".to_string()),
            ]),
        };
        assert_eq!(
            map_rag_result(result),
            ContextSourcePreview {
                title: "Design Notes".to_string(),
                uri: "file:///notes.md".to_string(),
                snippet: "local snippet".to_string(),
            }
        );
    }

    #[test]
    fn map_rag_result_falls_back_to_document_id() {
        let result = pb::SearchResult {
            document_id: "doc-1".to_string(),
            chunk_text: "local snippet".to_string(),
            score: 0.91,
            metadata: HashMap::new(),
        };
        let mapped = map_rag_result(result);
        assert_eq!(mapped.title, "doc-1");
        assert_eq!(mapped.uri, "doc-1");
    }

    #[test]
    fn augmented_prompt_is_noop_without_sources() {
        assert_eq!(build_augmented_prompt("hello", &[]), "hello");
    }

    #[test]
    fn augmented_prompt_adds_bounded_local_context_block() {
        let long = "x".repeat(LOCAL_CONTEXT_MAX_SNIPPET_CHARS + 4);
        let prompt = build_augmented_prompt(
            "answer this",
            &[ContextSourcePreview {
                title: "Doc".to_string(),
                uri: "file:///doc.md".to_string(),
                snippet: long,
            }],
        );
        assert!(prompt.starts_with("Local context:"));
        assert!(prompt.contains("[1] Doc"));
        assert!(prompt.contains("URI: file:///doc.md"));
        assert!(prompt.contains("xxx..."));
        assert!(prompt.ends_with("User request:\nanswer this"));
    }

    #[test]
    fn truncate_chars_keeps_short_values_unchanged() {
        assert_eq!(truncate_chars("short", 20), "short");
    }

    #[test]
    fn runtime_status_mapping_marks_reachable() {
        let status = pb::RuntimeStatus {
            version: "v1".to_string(),
            loaded_models: vec![pb::ModelInfo {
                id: "m".to_string(),
                name: "Model".to_string(),
                path: String::new(),
                size_bytes: 0,
                loaded: true,
                metadata: HashMap::new(),
            }],
            resources: None,
            healthy: true,
        };
        let mapped = map_runtime_status(status, true);
        assert!(mapped.reachable);
        assert!(mapped.healthy);
        assert_eq!(mapped.loaded_models.len(), 1);
    }

    #[test]
    fn default_runtime_parameters_match_existing_ollama_sampling() {
        let params = default_runtime_parameters();
        assert_eq!(params["temperature"], "1.0");
        assert_eq!(params["top_p"], "0.95");
        assert_eq!(params["top_k"], "64");
    }

    #[test]
    fn platform_binary_name_adds_exe_on_windows_only() {
        let name = platform_binary_name("ai-engine-server");
        assert_eq!(
            name,
            platform_binary_name_for("ai-engine-server", cfg!(windows))
        );
        assert_eq!(
            platform_binary_name_for("ai-engine-server", true),
            "ai-engine-server.exe"
        );
        assert_eq!(
            platform_binary_name_for("ai-engine-server", false),
            "ai-engine-server"
        );
    }

    #[test]
    fn candidate_paths_include_dev_submodule_binary() {
        let paths = candidate_engine_binary_paths(Path::new("repo"));
        assert!(paths.iter().any(|path| {
            path.ends_with(
                Path::new("external")
                    .join("rag-engine")
                    .join("engine")
                    .join("go")
                    .join("bin")
                    .join(platform_binary_name("ai-engine-server")),
            )
        }));
    }

    #[test]
    fn first_existing_file_returns_first_file_only() {
        let dir = std::env::temp_dir().join(format!(
            "thuki-engine-first-existing-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing");
        let directory = dir.join("directory");
        let file = dir.join("server");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(&file, "binary").unwrap();

        assert_eq!(
            first_existing_file(vec![missing, directory, file.clone()]),
            Some(file)
        );
    }

    #[test]
    fn first_existing_file_returns_none_without_files() {
        let dir = std::env::temp_dir().join(format!(
            "thuki-engine-first-existing-none-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing");
        let directory = dir.join("directory");
        std::fs::create_dir(&directory).unwrap();

        assert_eq!(first_existing_file(vec![missing, directory]), None);
    }
}
