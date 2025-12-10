//! # TiddlyWiki Server
//!
//! This is a web server for [TiddlyWiki]. It uses TiddlyWiki's [web server
//! API] to save tiddlers in a [SQLite database]. It should come  with a
//! slightly altered empty TiddlyWiki that includes an extra tiddler store (for
//! saved tiddlers) and  the `$:/plugins/tiddlywiki/tiddlyweb` plugin (which is
//! necessary to make use of the web server).
//!
//! [TiddlyWiki]: https://tiddlywiki.com/
//! [web server API]: https://tiddlywiki.com/#WebServer
//! [SQLite]: https://sqlite.org/index.html

use aws_config::{meta::region::RegionProviderChain, BehaviorVersion};
use aws_sdk_s3::{config::Credentials, config::Region, presigning::PresigningConfig, Client as S3Client};
use axum::{
    extract::{self, DefaultBodyLimit},
    http::StatusCode,
    routing::{delete, get, put},
    Extension, Router,
    middleware::{self, Next},
    response::Response, 
    http::{header},
    extract::Request,
};

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::fs;
use tokio::sync::Mutex;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{EnvFilter, layer::{self, SubscriberExt}, util::SubscriberInitExt};
use base64::{engine::general_purpose, Engine as _};
use tower_http::compression::CompressionLayer;

type DataStore = Arc<Mutex<Tiddlers>>;

// --- 配置结构定义 ---
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[derive(Deserialize, Debug, Clone)]
struct AppConfig {
    server: ServerConfig,
    s3: S3Config,
    #[serde(default = "default_status_config")] 
    status: Status, 
    auth: Option<AuthConfig>, 
}

fn default_status_config() -> Status {
    Status {
        username: "anonymous".to_string(),
        anonymous: false,
        read_only: false,
        space: Space::default(),
        tiddlywiki_version: default_tw_version(),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Status {
    // 只有 username 是我们主要想配的
    username: String,
    
    // 下面的字段如果有默认值，配置文件里可以省略
    #[serde(default)] 
    anonymous: bool,
    
    #[serde(default)]
    read_only: bool,
    
    #[serde(default)] 
    space: Space,
    
    #[serde(default = "default_tw_version")]
    tiddlywiki_version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Space {
    recipe: String,
}

// 为 Space 实现 Default，以便在配置文件省略时使用
impl Default for Space {
    fn default() -> Self {
        Self {
            recipe: "default".to_string(),
        }
    }
}

// 定义版本号的默认值生成函数
fn default_tw_version() -> String {
    "5.3.8".to_string()
}


#[derive(Deserialize, Debug, Clone)]
struct ServerConfig {
    bind: IpAddr,
    port: u16,
    db_path: PathBuf,
    files_dir: PathBuf,
}

#[derive(Deserialize, Debug, Clone)]
struct S3Config {
    enable: bool,
    access_key: String,
    secret_key: String,
    endpoint: String,
    region: String,
    bucket_name: String,
    public_url_base: String,
}

// [新增] 账号密码结构
#[derive(Deserialize, Debug, Clone)]
struct AuthConfig {
    username: String,
    password: String,
}

// --- 应用状态 ---

#[derive(Clone)]
struct AppState {
    s3_client: Option<S3Client>, // 设为 Option，允许不启用 S3
    bucket_name: String,
    public_url_base: String,
}

fn mime_to_ext(mime: &str) -> &str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

// --- 请求与响应结构 ---

#[derive(Deserialize)]
struct PresignRequest {
    filename: String,
    content_type: String,
}

#[derive(Serialize)]
struct PresignResponse {
    upload_url: String,
    public_url: String,
}

// --- 预处理模板 ---

#[derive(Clone)]
struct WikiTemplate {
    prefix: String,
    suffix: String,
}

impl WikiTemplate {
    fn new(html_content: &str) -> Self {
        let store_marker = r#"<script class="tiddlywiki-tiddler-store" type="application/json">"#;
        let start_tag_idx = html_content
            .find(store_marker)
            .expect("Invalid empty.html: missing store script tag");
        let end_tag_idx = html_content[start_tag_idx..]
            .find("</script>")
            .map(|i| start_tag_idx + i)
            .expect("Invalid empty.html: missing closing script tag");
        let split_idx = html_content[..end_tag_idx]
            .rfind(']')
            .expect("Invalid empty.html: store content is not a valid JSON array");

        Self {
            prefix: html_content[..split_idx].to_string(),
            suffix: html_content[split_idx..].to_string(),
        }
    }
}

// --- Handler: 获取 S3 预签名 URL ---
async fn get_presigned_url(
    Extension(state): Extension<Arc<AppState>>,
    extract::Query(params): extract::Query<PresignRequest>,
) -> AppResult<axum::Json<PresignResponse>> {
    let client = state.s3_client.as_ref().ok_or_else(|| {
        AppError::Response("S3 is not enabled in configuration".to_string())
    })?;

    let mut hasher = sha2::Sha256::new();
    hasher.update(params.filename.as_bytes());
    let ext = params.filename.split('.').last().unwrap_or("bin");
    let safe_key = format!("tiddlers/{}.{}", hex::encode(hasher.finalize()), ext);

    let presigned_req = client
        .put_object()
        .bucket(&state.bucket_name)
        .key(&safe_key)
        .content_type(&params.content_type)
        .presigned(PresigningConfig::expires_in(Duration::from_secs(300)).unwrap())
        .await
        .map_err(|e| AppError::Response(format!("S3 Presign failed: {}", e)))?;

    let upload_url = presigned_req.uri().to_string();
    let public_url = format!("{}/{}", state.public_url_base, safe_key);

    Ok(axum::Json(PresignResponse {
        upload_url,
        public_url,
    }))
}

// --- Main ---

#[tokio::main]
async fn main() {
    // 1. 初始化日志系统 (使用 tracing-subscriber)
    // 默认级别为 info，可以通过环境变量 RUST_LOG=debug 覆盖
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false) // 不显示模块路径，日志更清爽
                .compact(),         // 紧凑模式
        )
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // 2. 解析命令行参数并加载配置文件
    let args = Args::parse();
    let config_content = match fs::read_to_string(&args.config).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to read config file at {:?}: {}", args.config, e);
            return;
        }
    };
    
    let config: AppConfig = match toml::from_str(&config_content) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to parse config file: {}", e);
            return;
        }
    };
    
    tracing::info!("Configuration loaded from {:?}", args.config);

    // 3. 初始化数据库
    let datastore = initialize_datastore(&config.server).expect("Error initializing datastore");

    // 4. 加载 HTML 模板
    let empty_html_str = include_str!("../empty.html");
    let template = Arc::new(WikiTemplate::new(empty_html_str));

    // 5. 初始化 S3 客户端 (如果启用)
    let s3_client = if config.s3.enable {
        let credentials = Credentials::new(
            &config.s3.access_key,
            &config.s3.secret_key,
            None,
            None,
            "static_conf",
        );
        let region = Region::new(config.s3.region.clone());
        let s3_config = aws_config::defaults(BehaviorVersion::latest())
            .region(region)
            .credentials_provider(credentials)
            .endpoint_url(&config.s3.endpoint)
            .load()
            .await;
        
        tracing::info!("S3 client initialized for bucket: {}", config.s3.bucket_name);
        Some(S3Client::new(&s3_config))
    } else {
        tracing::warn!("S3 integration is disabled in config");
        None
    };

    let app_state = Arc::new(AppState {
        s3_client,
        bucket_name: config.s3.bucket_name.clone(),
        public_url_base: config.s3.public_url_base.clone(),
    });

    let files_service = ServeDir::new(&config.server.files_dir);
    let addr = SocketAddr::from((config.server.bind, config.server.port));

    // 6. 构建路由
    let app = Router::new()
        .route("/", get(render_wiki))
        .route("/status", get(status))
        .route("/recipes/default/tiddlers.json", get(all_tiddlers))
        .route(
            "/recipes/default/tiddlers/{title}",
            put(put_tiddler).get(get_tiddler),
        )
        .route("/bags/default/tiddlers/{title}", delete(delete_tiddler))
        .route("/bags/efault/tiddlers/{title}", delete(delete_tiddler)) // 兼容旧客户端拼写错误
        .route("/api/sign-upload", get(get_presigned_url))
        .nest_service("/files", files_service)
        .layer(Extension(datastore))
        .layer(Extension(config.server)) 
        .layer(Extension(template))
        .layer(Extension(app_state))
        .layer(Extension(Arc::new(config.status)))
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new().gzip(true).br(true).zstd(true))
        .layer(middleware::from_fn(auth_middleware))
        .layer(Extension(config.auth));
    tracing::info!("TiddlyWiki server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Error binding TCP listener");
    axum::serve(listener, app).await.expect("Error serving app");
}

fn initialize_datastore(config: &ServerConfig) -> AppResult<DataStore> {
    // 确保数据目录存在
    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Database(e.to_string()))?;
    }
    
    // 确保文件目录存在
    std::fs::create_dir_all(&config.files_dir).map_err(|e| AppError::Database(e.to_string()))?;

    let init_script = include_str!("./init.sql");
    let cxn = rusqlite::Connection::open(&config.db_path).map_err(AppError::from)?;
    cxn.execute_batch(init_script).map_err(AppError::from)?;

    const PLUGIN_JSON: &str = include_str!("../s3_uploader_plugin.json");
    let plugin_title = "$:/plugins/custom/s3-uploader";
    
    let exists: bool = cxn.query_row(
        "SELECT exists(SELECT 1 FROM tiddlers WHERE title = ?)",
        [plugin_title],
        |row| row.get(0)
    ).unwrap_or(false);

    if !exists {
        tracing::info!("Installing embedded S3 Uploader plugin...");
        let v: serde_json::Value = serde_json::from_str(PLUGIN_JSON)
            .map_err(|e| AppError::Serialization(format!("Invalid plugin json: {}", e)))?;
        
        let plugin_obj = if let serde_json::Value::Array(arr) = &v {
            arr.first().ok_or(AppError::Serialization("Empty json array".into()))?
        } else {
            &v
        };

        let tiddler = Tiddler::from_value(plugin_obj.clone())?;
        let mut stmt = cxn.prepare(
            "INSERT INTO tiddlers (title, revision, meta) VALUES (:title, :revision, :meta)"
        ).map_err(AppError::from)?;
        
        stmt.execute(rusqlite::named_params! {
            ":title": tiddler.title,
            ":revision": tiddler.revision,
            ":meta": tiddler.meta,
        }).map_err(AppError::from)?;
    }

    let tiddlers = Tiddlers { cxn };
    Ok(Arc::new(Mutex::new(tiddlers)))
}

// -----------------------------------------------------------------------------------
// Handlers

async fn render_wiki(
    Extension(ds): Extension<DataStore>,
    Extension(template): Extension<Arc<WikiTemplate>>,
) -> AppResult<axum::response::Response> {
    use axum::response::Response;

    let mut ds_lock = ds.lock().await;
    let datastore = &mut *ds_lock;

    let tiddlers: Vec<Tiddler> = datastore.all()?;
    let db_json_values: Vec<serde_json::Value> = tiddlers.iter().map(|t| t.as_value()).collect();
    let db_json_str = serde_json::to_string(&db_json_values)
        .map_err(|e| AppError::Serialization(format!("error serializing db: {}", e)))?;

    let inner_json = &db_json_str[1..db_json_str.len() - 1];
    let safe_json = inner_json.replace("</script>", "<\\/script>");

    let mut buffer = Vec::with_capacity(template.prefix.len() + safe_json.len() + template.suffix.len() + 1);
    buffer.extend(template.prefix.as_bytes());
    buffer.push(b',');
    buffer.extend(safe_json.as_bytes());
    buffer.extend(template.suffix.as_bytes());

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(buffer))
        .map_err(|e| AppError::Response(format!("error building wiki: {}", e)))
}

async fn all_tiddlers(Extension(ds): Extension<DataStore>) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let all: Vec<serde_json::Value> = tiddlers.all()?.iter().map(|t| t.as_skinny_value()).collect();
    Ok(axum::Json(all))
}

async fn get_tiddler(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
) -> AppResult<axum::http::Response<String>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;

    if let Some(t) = tiddlers.get(&title)? {
        let body = serde_json::to_string_pretty(&t.as_value())
            .map_err(|e| AppError::Serialization(format!("error serializing tiddler: {}", e)))?;
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(body)
            .map_err(|e| AppError::Response(format!("error building response: {}", e)))
    } else {
        axum::response::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(String::new())
            .map_err(|e| AppError::Response(format!("error building 404 response: {}", e)))
    }
}

async fn delete_tiddler(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
) -> AppResult<axum::response::Response<String>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    tiddlers.pop(&title)?;
    
    // 记录删除操作
    tracing::info!("Deleted tiddler: {}", title);

    let mut resp = axum::response::Response::default();
    *resp.status_mut() = StatusCode::NO_CONTENT;
    Ok(resp)
}

async fn put_tiddler(
    Extension(ds): Extension<DataStore>,
    Extension(config): Extension<ServerConfig>, // 注意这里改成了 ServerConfig
    extract::Path(title): extract::Path<String>,
    extract::Json(mut v): extract::Json<serde_json::Value>,
) -> AppResult<axum::http::Response<String>> {
    use axum::http::response::Response;

    let is_binary = if let Some(type_val) = v.get("type") {
        let t = type_val.as_str().unwrap_or("");
        t.starts_with("image/") || t == "application/pdf" || t.starts_with("video/") || t.starts_with("audio/")
    } else {
        false
    };

    if is_binary {
        if let Some(text_val) = v.get("text") {
            if let Some(base64_str) = text_val.as_str() {
                if !base64_str.is_empty() {
                    let clean_b64 = if let Some(idx) = base64_str.find(",") {
                        &base64_str[idx + 1..]
                    } else {
                        base64_str
                    };

                    if let Ok(data) = general_purpose::STANDARD.decode(clean_b64) {
                        let mut hasher = Sha256::new();
                        hasher.update(title.as_bytes());
                        let safe_filename = hex::encode(hasher.finalize());
                        let mime = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        let ext = mime_to_ext(mime);
                        let filename = format!("{}.{}", safe_filename, ext);
                        let file_path = config.files_dir.join(&filename);

                        if let Err(e) = fs::write(&file_path, &data).await {
                            tracing::error!("Failed to write file to disk: {}", e);
                        } else {
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("text".to_string(), serde_json::Value::String("".to_string()));
                                let uri = format!("/files/{}", filename);
                                obj.insert("_canonical_uri".to_string(), serde_json::Value::String(uri));
                                tracing::info!("Offloaded binary file for '{}' to {}", title, file_path.display());
                            }
                        }
                    }
                }
            }
        }
    }

    let mut new_tiddler = Tiddler::from_value(v)?;
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;

    if let Some(_old_tiddler) = tiddlers.pop(&title)? {
        new_tiddler.revision += 1;
    }
    let new_revision = new_tiddler.revision;
    tiddlers.put(new_tiddler)?;
    
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Etag", format!("default/{}/{}:", title, new_revision))
        .body(String::new())
        .map_err(|e| AppError::Response(format!("Error building response: {}", e)))
}

// -----------------------------------------------------------------------------------
// Models
pub(crate) struct Tiddlers {
    cxn: rusqlite::Connection,
}

impl Tiddlers {
    pub(crate) fn all(&self) -> AppResult<Vec<Tiddler>> {
        // 将 debug 改为 trace 减少刷屏
        tracing::trace!("Retrieving all tiddlers"); 
        const GET: &str = r#"SELECT title, revision, meta FROM tiddlers"#;
        let mut stmt = self.cxn.prepare_cached(GET).map_err(AppError::from)?;
        let raw_tiddlers = stmt
            .query_map([], |r| r.get::<usize, serde_json::Value>(2))
            .map_err(AppError::from)?;
        let mut tiddlers = Vec::new();
        for qt in raw_tiddlers {
            let raw = qt.map_err(AppError::from)?;
            tiddlers.push(Tiddler::from_value(raw)?);
        }
        Ok(tiddlers)
    }

    pub(crate) fn get(&self, title: &str) -> AppResult<Option<Tiddler>> {
        use rusqlite::OptionalExtension;
        tracing::debug!("getting tiddler: {}", title);
        const GET: &str = r#"SELECT title, revision, meta FROM tiddlers WHERE title = ?"#;
        let raw = self
            .cxn
            .query_row(GET, [title], |r| r.get::<usize, serde_json::Value>(2))
            .optional()
            .map_err(|e| AppError::Database(format!("Error retrieving '{}': {}", title, e)))?;
        raw.map(Tiddler::from_value).transpose()
    }

    pub(crate) fn put(&mut self, tiddler: Tiddler) -> AppResult<()> {
        tracing::debug!("putting tiddler: {}", tiddler.title);
        const PUT: &str = r#"
            INSERT INTO tiddlers (title, revision, meta) VALUES (:title, :revision, :meta)
            ON CONFLICT (title) DO UPDATE
            SET title = :title, revision = :revision, meta = :meta
        "#;
        let mut stmt = self.cxn.prepare_cached(PUT).map_err(|e| AppError::Database(format!("Error preparing statement: {}", e)))?;
        stmt.execute(rusqlite::named_params! {
            ":title": tiddler.title,
            ":revision": tiddler.revision,
            ":meta": tiddler.meta,
        })?;
        Ok(())
    }

    pub(crate) fn pop(&mut self, title: &str) -> AppResult<Option<Tiddler>> {
        tracing::debug!("popping tiddler: {}", title);
        let result = self.get(title)?;
        const DELETE: &str = "DELETE FROM tiddlers WHERE title = :title";
        let mut stmt = self.cxn.prepare(DELETE).map_err(|e| AppError::Database(format!("Error preparing {}: {}", DELETE, e)))?;
        stmt.execute(rusqlite::named_params! { ":title": title })
            .map_err(|e| AppError::Database(format!("Error removing tiddler: {}", e)))?;
        Ok(result)
    }
}

#[derive(Clone, Serialize, Debug)]
pub(crate) struct Tiddler {
    title: String,
    revision: u64,
    meta: serde_json::Value,
}

impl Tiddler {
    pub(crate) fn as_value(&self) -> Value {
        let mut meta = self.meta.clone();
        if let Value::Object(ref mut map) = meta {
            if let Some(Value::Object(fields)) = map.remove("fields") {
                for (k, v) in fields {
                    map.entry(k).or_insert(v);
                }
            }
            if let Some(tags_val) = map.get("tags") {
                match tags_val {
                    Value::Array(arr) => {
                        let tag_str = arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| if s.contains(' ') { format!("[[{}]]", s) } else { s.to_string() })
                            .collect::<Vec<String>>()
                            .join(" ");
                        map.insert("tags".to_string(), Value::String(tag_str));
                    },
                    Value::String(_) => {},
                    _ => { map.remove("tags"); }
                }
            }
            map.insert("title".to_string(), Value::String(self.title.clone()));
            map.insert("revision".to_string(), Value::String(self.revision.to_string()));
            map.entry("bag".to_string()).or_insert(Value::String("default".to_string()));
        }
        meta
    }

    pub(crate) fn as_skinny_value(&self) -> Value {
        let meta = self.as_value();
        if let Value::Object(mut map) = meta {
            map.remove("text");
            Value::Object(map)
        } else {
            meta
        }
    }

    pub(crate) fn from_value(value: Value) -> AppResult<Tiddler> {
        let obj = match value.clone() {
            Value::Object(m) => m,
            _ => return Err(AppError::Serialization("from_value expects a JSON Object".to_string())),
        };
        let title = match obj.get("title") {
            Some(Value::String(s)) => s,
            _ => return Err(AppError::Serialization("tiddler['title'] should be a string".to_string())),
        };
        let revision = match obj.get("revision") {
            None => 0,
            Some(Value::Number(n)) => n.as_u64().ok_or_else(|| AppError::Serialization(format!("revision should be a u64 (not {})", n)))?,
            Some(Value::String(s)) => s.parse::<u64>().map_err(|_| AppError::Serialization(format!("couldn't parse a revision number from '{}'", s)))?,
            _ => return Err(AppError::Serialization("tiddler['revision'] should be a number".to_string())),
        };
        Ok(Tiddler { title: title.clone(), revision, meta: value })
    }
}

// -----------------------------------------------------------------------------------

async fn status(Extension(status_config): Extension<Arc<Status>>) -> axum::Json<Status> {
    // axum::Json(STATUS)
    axum::Json(status_config.as_ref().clone())
}

// -----------------------------------------------------------------------------------
// Error handling

type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
enum AppError {
    Database(String),
    Response(String),
    Serialization(String),
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!("{:?}", self);
        let msg = match self {
            AppError::Database(msg) => msg,
            AppError::Response(msg) => msg,
            AppError::Serialization(msg) => msg,
        };
        (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> AppError {
        tracing::error!("{:?}", err);
        AppError::Database(err.to_string())
    }
}

async fn auth_middleware(
    Extension(auth_config): Extension<Option<AuthConfig>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // 1. 如果配置中没有 auth 部分，直接放行 (允许无密码运行)
    let auth = match auth_config {
        Some(config) => config,
        None => return Ok(next.run(req).await),
    };

    // 2. 获取请求头中的 Authorization
    let auth_header = req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Basic "));

    // 3. 验证账号密码
    if let Some(encoded) = auth_header {
        // 解码 Base64
        if let Ok(decoded) = general_purpose::STANDARD.decode(encoded) {
            if let Ok(creds) = String::from_utf8(decoded) {
                // 格式通常是 "username:password"
                if let Some((u, p)) = creds.split_once(':') {
                    if u == auth.username && p == auth.password {
                        // 验证通过，继续处理请求
                        return Ok(next.run(req).await);
                    }
                }
            }
        }
    }

    // 4. 验证失败或未提供 Header，返回 401 并触发浏览器弹窗
    tracing::warn!("Unauthorized access attempt");
    let response = Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Basic realm=\"TiddlyWiki Server\"")
        .body(axum::body::Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(response)
}