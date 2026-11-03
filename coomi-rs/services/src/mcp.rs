use anyhow::Context;
use anyhow::Result;
use coomi_engine::ToolResult;
use coomi_engine::ToolSpec;
use reqwest::Client;
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Duration;

#[derive(Clone, Debug, serde::Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub transport: String,
    pub enabled: bool,
    pub tools_count: usize,
    pub error: Option<String>,
}

pub struct McpRuntime {
    tools: RwLock<BTreeMap<String, McpTool>>,
    statuses: RwLock<Vec<McpServerStatus>>,
}

impl Default for McpRuntime {
    fn default() -> Self {
        Self {
            tools: RwLock::new(BTreeMap::new()),
            statuses: RwLock::new(Vec::new()),
        }
    }
}

#[derive(Clone)]
struct McpTool {
    spec: ToolSpec,
    original_name: String,
    client: McpClient,
}

#[derive(Clone)]
enum McpClient {
    Stdio(Arc<Mutex<StdioClient>>),
    Http(HttpClient),
    Sse(Arc<Mutex<SseClient>>),
}

#[derive(Clone)]
struct HttpClient {
    url: String,
    headers: BTreeMap<String, String>,
    client: Client,
}

struct StdioClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
    next_id: u64,
    framing: StdioFraming,
}

struct SseClient {
    url: Url,
    headers: BTreeMap<String, String>,
    client: Client,
    messages: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
}

#[derive(Clone, Copy)]
enum StdioFraming {
    Lines,
    ContentLength,
}

#[derive(Deserialize)]
struct McpDocument {
    #[serde(default)]
    servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct McpServerConfig {
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub framing: String,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: String::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            framing: String::new(),
        }
    }
}

fn default_transport() -> String {
    "stdio".into()
}

const fn default_true() -> bool {
    true
}

impl McpRuntime {
    pub async fn load(home: &Path) -> Self {
        let path = home.join("config").join("mcp_servers.json");
        let document = match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice::<McpDocument>(&bytes) {
                Ok(document) => document,
                Err(error) => {
                    return Self {
                        statuses: RwLock::new(vec![McpServerStatus {
                            name: "configuration".into(),
                            transport: "file".into(),
                            enabled: true,
                            tools_count: 0,
                            error: Some(format!("invalid {}: {error}", path.display())),
                        }]),
                        ..Self::default()
                    };
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                return Self {
                    statuses: RwLock::new(vec![McpServerStatus {
                        name: "configuration".into(),
                        transport: "file".into(),
                        enabled: true,
                        tools_count: 0,
                        error: Some(format!("failed to read {}: {error}", path.display())),
                    }]),
                    ..Self::default()
                };
            }
        };

        let runtime = Self::default();
        for (name, config) in document.servers {
            if !config.enabled {
                runtime
                    .statuses
                    .write()
                    .expect("MCP status lock poisoned")
                    .push(McpServerStatus {
                        name,
                        transport: config.transport,
                        enabled: false,
                        tools_count: 0,
                        error: None,
                    });
                continue;
            }
            match connect(&name, &config).await {
                Ok((client, specs)) => {
                    let count = specs.len();
                    let mut tools = runtime.tools.write().expect("MCP tool lock poisoned");
                    for (original_name, description, parameters) in specs {
                        let flat = mcp_tool_name(&name, &original_name);
                        tools.entry(flat.clone()).or_insert(McpTool {
                            spec: ToolSpec {
                                name: flat,
                                description,
                                parameters,
                            },
                            original_name,
                            client: client.clone(),
                        });
                    }
                    drop(tools);
                    runtime
                        .statuses
                        .write()
                        .expect("MCP status lock poisoned")
                        .push(McpServerStatus {
                            name,
                            transport: config.transport,
                            enabled: true,
                            tools_count: count,
                            error: None,
                        });
                }
                Err(error) => runtime
                    .statuses
                    .write()
                    .expect("MCP status lock poisoned")
                    .push(McpServerStatus {
                        name,
                        transport: config.transport,
                        enabled: true,
                        tools_count: 0,
                        error: Some(format!("{error:#}")),
                    }),
            }
        }
        runtime
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .read()
            .expect("MCP tool lock poisoned")
            .values()
            .map(|tool| tool.spec.clone())
            .collect()
    }

    /// Reconnect MCP servers and replace the live tool inventory without restarting the agent.
    pub async fn reload(&self, home: &Path) {
        let fresh = Self::load(home).await;
        *self.tools.write().expect("MCP tool lock poisoned") =
            fresh.tools.into_inner().unwrap_or_default();
        *self.statuses.write().expect("MCP status lock poisoned") =
            fresh.statuses.into_inner().unwrap_or_default();
    }

    /// 已配置 MCP server 清单（含每个 server 的可调用工具名），供系统提示注入：
    /// agent 需要知道「装了哪些 MCP、能不能用、能调哪些工具」才能方便地调用。
    /// 连接失败的 server 也列出（带错误原因），避免 agent 以为它不存在。
    pub fn inventory(&self) -> String {
        let tools = self.tools.read().expect("MCP tool lock poisoned");
        let statuses = self.statuses.read().expect("MCP status lock poisoned");
        if statuses.is_empty() && tools.is_empty() {
            return String::new();
        }
        let mut out = String::from("Configured MCP servers:\n");
        for status in statuses.iter() {
            let prefix = format!("mcp__{}__", sanitize(&status.name));
            let mut tool_names: Vec<&String> = tools
                .keys()
                .filter(|name| name.starts_with(&prefix))
                .collect();
            tool_names.sort();
            let state = if !status.enabled {
                "disabled".to_string()
            } else if let Some(error) = &status.error {
                format!("connect error: {error}")
            } else {
                "enabled".to_string()
            };
            let tools = if tool_names.is_empty() {
                "no tools".to_string()
            } else {
                tool_names
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out.push_str(&format!(
                "- {} ({}): {}, tools: {}\n",
                status.name, status.transport, state, tools
            ));
        }
        out
    }

    pub fn statuses(&self) -> Vec<McpServerStatus> {
        self.statuses
            .read()
            .expect("MCP status lock poisoned")
            .clone()
    }

    /// 测试 MCP server 连接，不保存配置也不污染 live runtime。
    pub async fn test_config(&self, name: &str, config: &McpServerConfig) -> McpServerStatus {
        match connect(name, config).await {
            Ok((_, specs)) => McpServerStatus {
                name: name.to_string(),
                transport: config.transport.clone(),
                enabled: config.enabled,
                tools_count: specs.len(),
                error: None,
            },
            Err(error) => McpServerStatus {
                name: name.to_string(),
                transport: config.transport.clone(),
                enabled: config.enabled,
                tools_count: 0,
                error: Some(format!("{error:#}")),
            },
        }
    }

    pub async fn call(&self, name: &str, arguments: Value) -> Option<ToolResult> {
        let hit = {
            let tools = self.tools.read().expect("MCP tool lock poisoned");
            // 模型可能用完整扁平名（mcp__server__tool）或只写 server__tool / tool 名，
            // 逐个尝试解析；命中后 clone 出来再 drop 锁（锁不能跨 await）。
            let bare = name
                .strip_prefix("mcp__")
                .map(|rest| rest.split("__").last().unwrap_or(rest))
                .unwrap_or(name);
            tools.get(name).cloned().or_else(|| {
                tools
                    .values()
                    .find(|tool| {
                        tool.original_name == bare
                            || tool.spec.name == name
                            || tool.spec.name.ends_with(&format!("__{bare}"))
                    })
                    .cloned()
            })?
        };
        let result = hit
            .client
            .request(
                "tools/call",
                json!({"name": hit.original_name, "arguments": arguments}),
            )
            .await;
        Some(match result {
            Ok(value) => {
                let is_error = value
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let output = mcp_content_text(&value);
                if is_error {
                    ToolResult::error(output)
                } else {
                    ToolResult::success(output)
                }
            }
            Err(error) => ToolResult::error(format!("MCP call failed: {error:#}")),
        })
    }
}

async fn connect(
    server_name: &str,
    config: &McpServerConfig,
) -> Result<(McpClient, Vec<(String, String, Value)>)> {
    let client = match config.transport.to_ascii_lowercase().as_str() {
        "stdio" => McpClient::Stdio(Arc::new(Mutex::new(StdioClient::start(config).await?))),
        "http" => {
            anyhow::ensure!(!config.url.trim().is_empty(), "MCP server has no URL");
            McpClient::Http(HttpClient {
                url: config.url.clone(),
                headers: config.headers.clone(),
                client: Client::new(),
            })
        }
        "sse" => McpClient::Sse(Arc::new(Mutex::new(SseClient::connect(config).await?))),
        transport => anyhow::bail!("unsupported MCP transport: {transport}"),
    };
    let initialize = client
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "coomi", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await
        .with_context(|| format!("failed to initialize MCP `{server_name}`"))?;
    let _ = initialize;
    client
        .notify("notifications/initialized", json!({}))
        .await?;
    let listed = client.request("tools/list", json!({})).await?;
    let tools = listed
        .get("tools")
        .and_then(Value::as_array)
        .context("MCP tools/list result has no tools array")?
        .iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .context("MCP tool has no name")?
                .to_owned();
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("MCP tool")
                .to_owned();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            Ok((name, description, parameters))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((client, tools))
}

impl McpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        match self {
            Self::Stdio(client) => client.lock().await.request(method, params).await,
            Self::Http(client) => client.request(method, params).await,
            Self::Sse(client) => client.lock().await.request(method, params).await,
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        match self {
            Self::Stdio(client) => client.lock().await.notify(method, params).await,
            Self::Http(client) => client.notify(method, params).await,
            Self::Sse(client) => client.lock().await.notify(method, params).await,
        }
    }
}

impl StdioClient {
    async fn start(config: &McpServerConfig) -> Result<Self> {
        anyhow::ensure!(
            !config.command.trim().is_empty(),
            "MCP server has no command"
        );
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !config.cwd.trim().is_empty() {
            command.current_dir(PathBuf::from(&config.cwd));
        }
        let mut child = command.spawn().context("failed to start MCP process")?;
        let stdin = child.stdin.take().context("MCP process has no stdin")?;
        let stdout = child.stdout.take().context("MCP process has no stdout")?;
        let mut stderr = child.stderr.take().context("MCP process has no stderr")?;
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let task_buffer = Arc::clone(&stderr_buffer);
        tokio::spawn(async move {
            let mut output = Vec::new();
            let _ = stderr.read_to_end(&mut output).await;
            if output.len() > 64 * 1024 {
                output = output.split_off(output.len() - 64 * 1024);
            }
            *task_buffer.lock().await = output;
        });
        Ok(Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr: stderr_buffer,
            next_id: 1,
            framing: if config.framing.eq_ignore_ascii_case("content_length")
                || config.framing.eq_ignore_ascii_case("content-length")
            {
                StdioFraming::ContentLength
            } else {
                StdioFraming::Lines
            },
        })
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        loop {
            let message = self.read().await?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                anyhow::bail!("MCP error: {error}")
            }
            return message
                .get("result")
                .cloned()
                .context("MCP response has no result");
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn write(&mut self, value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        match self.framing {
            StdioFraming::Lines => {
                self.stdin.write_all(&bytes).await?;
                self.stdin.write_all(b"\n").await?;
            }
            StdioFraming::ContentLength => {
                self.stdin
                    .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
                    .await?;
                self.stdin.write_all(&bytes).await?;
            }
        }
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line).await? == 0 {
                return Err(self.process_closed_error("MCP process closed stdout").await);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(length) = parse_content_length_header(trimmed)? {
                loop {
                    line.clear();
                    if self.stdout.read_line(&mut line).await? == 0 {
                        return Err(self
                            .process_closed_error("MCP process closed during frame headers")
                            .await);
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                }
                anyhow::ensure!(length <= 8 * 1024 * 1024, "MCP frame exceeds 8 MiB");
                let mut body = vec![0; length];
                self.stdout.read_exact(&mut body).await?;
                return serde_json::from_slice(&body)
                    .context("invalid framed MCP JSON-RPC response");
            }
            return serde_json::from_str(trimmed).context("invalid MCP JSON-RPC response");
        }
    }

    async fn process_closed_error(&self, message: &str) -> anyhow::Error {
        tokio::task::yield_now().await;
        let stderr = self.stderr.lock().await;
        let diagnostic = String::from_utf8_lossy(&stderr).trim().to_owned();
        if diagnostic.is_empty() {
            anyhow::anyhow!(message.to_owned())
        } else {
            anyhow::anyhow!("{message}: {diagnostic}")
        }
    }
}

impl HttpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let mut builder = self.client.post(&self.url);
        for (key, value) in &self.headers {
            builder = builder.header(key, value);
        }
        let response = builder
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await?
            .error_for_status()?;
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let value: Value = if is_sse {
            parse_sse_response(&response.text().await?)?
        } else {
            response.json().await?
        };
        if let Some(error) = value.get("error") {
            anyhow::bail!("MCP error: {error}")
        }
        value
            .get("result")
            .cloned()
            .context("MCP response has no result")
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let mut builder = self.client.post(&self.url);
        for (key, value) in &self.headers {
            builder = builder.header(key, value);
        }
        builder
            .json(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

impl SseClient {
    async fn connect(config: &McpServerConfig) -> Result<Self> {
        anyhow::ensure!(!config.url.trim().is_empty(), "MCP SSE server has no URL");
        let base_url = Url::parse(&config.url).context("invalid MCP SSE URL")?;
        let client = Client::new();
        let mut builder = client
            .get(base_url.clone())
            .header("accept", "text/event-stream");
        for (key, value) in &config.headers {
            builder = builder.header(key, value);
        }
        let response = builder.send().await?.error_for_status()?;
        let (endpoint_tx, endpoint_rx) = oneshot::channel();
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        tokio::spawn(read_sse_stream(response, endpoint_tx, message_tx));
        let endpoint = tokio::time::timeout(Duration::from_secs(15), endpoint_rx)
            .await
            .context("MCP SSE server did not provide an endpoint")??;
        let url = base_url
            .join(&endpoint)
            .context("invalid MCP SSE endpoint")?;
        anyhow::ensure!(
            url.scheme() == base_url.scheme()
                && url.host_str() == base_url.host_str()
                && url.port_or_known_default() == base_url.port_or_known_default(),
            "MCP SSE endpoint changed origin"
        );
        Ok(Self {
            url,
            headers: config.headers.clone(),
            client,
            messages: message_rx,
            next_id: 1,
        })
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.post(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        loop {
            let message = tokio::time::timeout(Duration::from_secs(30), self.messages.recv())
                .await
                .context("MCP SSE request timed out")?
                .context("MCP SSE stream closed")?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                anyhow::bail!("MCP error: {error}")
            }
            return message
                .get("result")
                .cloned()
                .context("MCP response has no result");
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.post(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn post(&self, value: Value) -> Result<()> {
        let mut builder = self.client.post(self.url.clone());
        for (key, value) in &self.headers {
            builder = builder.header(key, value);
        }
        builder.json(&value).send().await?.error_for_status()?;
        Ok(())
    }
}

async fn read_sse_stream(
    response: reqwest::Response,
    endpoint: oneshot::Sender<String>,
    messages: mpsc::UnboundedSender<Value>,
) {
    use futures_util::StreamExt;

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut endpoint = Some(endpoint);
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some((end, delimiter_len)) = next_sse_event(&buffer) {
            let event = buffer[..end].to_owned();
            buffer.drain(..end + delimiter_len);
            let (kind, data) = parse_sse_event(&event);
            if kind == "endpoint" {
                if let Some(sender) = endpoint.take() {
                    let _ = sender.send(data);
                }
            } else if !data.is_empty()
                && let Ok(value) = serde_json::from_str(&data)
            {
                let _ = messages.send(value);
            }
        }
    }
}

fn next_sse_event(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\n\n"), buffer.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (_, Some(crlf)) => Some((crlf, 4)),
        _ => None,
    }
}

fn parse_sse_event(event: &str) -> (String, String) {
    let mut kind = "message".to_owned();
    let mut data = Vec::new();
    for line in event.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            kind = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_owned());
        }
    }
    (kind, data.join("\n"))
}

fn parse_sse_response(body: &str) -> Result<Value> {
    let normalized = body.replace("\r\n", "\n");
    let values = normalized
        .split("\n\n")
        .flat_map(|event| {
            let (_, data) = parse_sse_event(event);
            serde_json::from_str::<Value>(&data)
        })
        .collect::<Vec<_>>();
    values
        .into_iter()
        .find(|value| value.get("result").is_some() || value.get("error").is_some())
        .context("MCP SSE response contained no JSON-RPC result")
}

fn parse_content_length_header(line: &str) -> Result<Option<usize>> {
    let Some((name, value)) = line.split_once(':') else {
        return Ok(None);
    };
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return Ok(None);
    }
    let length = value
        .trim()
        .parse::<usize>()
        .context("invalid MCP Content-Length header")?;
    anyhow::ensure!(length <= 8 * 1024 * 1024, "MCP frame exceeds 8 MiB");
    Ok(Some(length))
}

fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", sanitize(server), sanitize(tool))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn mcp_content_text(value: &Value) -> String {
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("text") => item.get("text").and_then(Value::as_str).map(str::to_owned),
            _ => Some(item.to_string()),
        })
        .collect::<Vec<_>>();
    if content.is_empty() {
        value.to_string()
    } else {
        content.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_stable_flat_tool_names() {
        assert_eq!(
            mcp_tool_name("Git Hub", "issues/list"),
            "mcp__git_hub__issues_list"
        );
    }

    #[test]
    fn parses_sse_with_crlf_and_multiline_fields() {
        let value = parse_sse_response(
            "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\r\n\r\n",
        )
        .expect("SSE response");
        assert_eq!(value["id"], 1);
        assert_eq!(next_sse_event("event: x\r\ndata: y\r\n\r\n"), Some((17, 4)));
    }

    #[test]
    fn parses_content_length_framing_case_insensitively() {
        assert_eq!(
            parse_content_length_header("CONTENT-LENGTH: 42").expect("header"),
            Some(42)
        );
        assert_eq!(
            parse_content_length_header("Content-Type: application/json").expect("other header"),
            None
        );
        assert!(parse_content_length_header("content-length: invalid").is_err());
    }
}
