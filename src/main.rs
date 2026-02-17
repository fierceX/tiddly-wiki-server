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
    Extension, Router, extract::{self, DefaultBodyLimit, Request}, http::{StatusCode, header}, middleware::{self, Next}, response::Response, routing::{delete, get, post, put}
};

use axum::{
    body::Body,
    extract::Path,
    response::{IntoResponse},
};

use axum::http::{HeaderValue, header::CONTENT_SECURITY_POLICY};
use chrono::Local;
use tower_http::set_header::SetResponseHeaderLayer; // 引入修改响应头的层
use clap::Parser;
use rusqlite::params;
use rusqlite::Connection;
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

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/foliate-js/ebook_reader/"] // 编译时，Cargo 会去这个路径把文件打包进来
struct FoliateAssets;


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
    name:String,
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
    s3_name:String,
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
    name:String,
    key: String,       
    bucket: String,
    region: String,
}


use std::collections::HashMap;

#[derive(Deserialize, Debug)] // 建议加上 Debug 以便调试
struct InboxRequest {
    #[serde(rename = "type")] // 将 JSON 中的 "type" 映射为 item_type
    item_type: String,
    
    title: String,
    
    tags: Vec<String>, // 现在接收字符串数组
    
    #[serde(rename = "content")] // 将 JSON 中的 "content" 映射为 text
    text: String,
    
    timestamp: String, // ISO 8601 格式字符串
    
    #[serde(default)]
    context: Option<String>,
    
    #[serde(default)]
    metadata: Option<HashMap<String, serde_json::Value>>,
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

    let region = client.config().region().map(|r| r.as_ref()).unwrap_or("default").to_string();

    Ok(axum::Json(PresignResponse {
        upload_url,
        public_url,
        name:state.s3_name.clone(),
        key: safe_key,
        bucket: state.bucket_name.clone(),
        region,
    }))
}


// 处理 /foliate/* 的请求
async fn static_handler(Path(path): Path<String>) -> impl IntoResponse {
    // 1. 从嵌入资源中尝试获取文件
    match FoliateAssets::get(path.as_str()) {
        Some(content) => {
            // 2. 猜测 MIME 类型 (例如 index.html -> text/html)
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            
            // 3. 构建响应
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                // 可以根据需要添加缓存头，因为是内嵌文件，甚至可以缓存很久
                .header(header::CACHE_CONTROL, "public, max-age=3600") 
                .body(Body::from(content.data))
                .unwrap()
        }
        None => {
            // 4. 找不到文件返回 404
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("404 Not Found"))
                .unwrap()
        }
    }
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
        s3_name:config.s3.name.clone(),
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
        .route("/api/inbox", post(add_inbox_item))
        .nest_service("/files", files_service)
        // .nest_service("/foliate", epub_service)
        .route("/foliate/{*path}", get(static_handler)) 
        
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

fn insert_default_data(str:&str,conn: &Connection) -> Result<(), AppError> {
    tracing::info!("Installing plugin...");
    let v: serde_json::Value = serde_json::from_str(str)
        .map_err(|e| AppError::Serialization(format!("Invalid plugin json: {}", e)))?;
    
    let plugin_obj = if let serde_json::Value::Array(arr) = &v {
        arr.first().ok_or(AppError::Serialization("Empty json array".into()))?
    } else {
        &v
    };

    let tiddler = Tiddler::from_value(plugin_obj.clone())?;
    let mut stmt = conn.prepare(
        "INSERT INTO tiddlers (title, revision, meta) VALUES (:title, :revision, :meta)"
    ).map_err(AppError::from)?;
    
    stmt.execute(rusqlite::named_params! {
        ":title": tiddler.title,
        ":revision": tiddler.revision,
        ":meta": tiddler.meta,
    }).map_err(AppError::from)?;
    Ok(())
}

fn initialize_datastore(config: &ServerConfig) -> AppResult<DataStore> {
    // 确保数据目录存在
    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Database(e.to_string()))?;
    }
    
    // 确保文件目录存在
    std::fs::create_dir_all(&config.files_dir).map_err(|e| AppError::Database(e.to_string()))?;

    // 检查数据库文件是否存在
    let db_exists = config.db_path.exists();

    // 打开数据库连接
    let cxn = Connection::open(&config.db_path).map_err(AppError::from)?;

    // 只有在数据库不存在时才执行初始化
    if !db_exists {
        const S3_PLUGIN_JSON: &str = include_str!("../s3_uploader_plugin.json");
        const CPL_PLUGIN_JSON: &str = include_str!("../CPL-Repo.json");
        // 开启 WAL 模式
        cxn.execute_batch(r#"
                            PRAGMA journal_mode = WAL;
                            PRAGMA synchronous = FULL;
                            PRAGMA busy_timeout = 5000;
                            PRAGMA cache_size = -5000;
                            PRAGMA mmap_size = 67108864;
                            PRAGMA page_size = 4096;
                            PRAGMA temp_store = MEMORY;
                            PRAGMA journal_size_limit = 33554432;
                            PRAGMA wal_checkpoint(TRUNCATE);"#)
            .map_err(AppError::from)?;
        
        // 执行初始化 SQL 脚本
        let init_script = include_str!("./init.sql");
        
        cxn.execute_batch(init_script)
            .map_err(|e| AppError::Database(format!("初始化数据库失败: {}", e)))?;
        insert_default_data(S3_PLUGIN_JSON,&cxn)?;
        insert_default_data(CPL_PLUGIN_JSON,&cxn)?;
        
        tracing::info!("The database initialization has been completed.")
    } else {
        tracing::info!("Use the existing database!")
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
    Extension(state): Extension<Arc<AppState>>,
    Extension(config): Extension<ServerConfig>,
    extract::Path(title): extract::Path<String>,
) -> AppResult<axum::response::Response<String>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let deleted_tiddler = tiddlers.pop(&title)?;
    drop(lock);
    // tiddlers.pop(&title)?;
    // 如果成功删除了条目，检查是否有关联文件需要删除
    if let Some(tiddler) = deleted_tiddler {
        // 这里我们使用 tokio::spawn 异步后台删除，不阻塞 HTTP 响应
        // 如果你希望确认文件删除后再返回，可以去掉 spawn 直接 await
        tokio::spawn(async move {
            try_delete_associated_file(tiddler, state, config).await;
        });
    }
    // 记录删除操作
    tracing::info!("Deleted tiddler: {}", title);

    let mut resp = axum::response::Response::default();
    *resp.status_mut() = StatusCode::NO_CONTENT;
    Ok(resp)
}

async fn try_delete_associated_file(tiddler: Tiddler, state: Arc<AppState>, config: ServerConfig) {
    // 1. 尝试从 meta 中提取 _canonical_uri
    // Tiddler 的 JSON 结构中，字段可能在顶层，也可能在 'fields' 对象里
    let uri = match tiddler.meta.get("_canonical_uri") {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => tiddler.meta.get("fields")
            .and_then(|f| f.get("_canonical_uri"))
            .and_then(|v| v.as_str())
    };

    let uri = match uri {
        Some(u) => u,
        None => return, // 没有外部文件链接，直接返回
    };

    let get_field = |key: &str| -> Option<String> {
        tiddler.meta.get(key)
            .or_else(|| tiddler.meta.get("fields").and_then(|f| f.get(key)))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    tracing::debug!("Found associated file URI: {}", uri);

    // 1. 优先检查 _file_storage 标记
    let storage_type = get_field("_file_storage");

    // === 分支 A: 明确标记为 S3 存储 ===
    if storage_type.as_deref() == Some("s3") {
        if let Some(client) = &state.s3_client {
            // 获取 bucket 和 key，如果字段不存在则无法删除
            let bucket = get_field("_s3_bucket").unwrap_or_else(|| state.bucket_name.clone());
            let key = match get_field("_s3_key") {
                Some(k) => k,
                None => {
                    tracing::warn!("Tiddler marked as S3 but missing _s3_key: {}", tiddler.title);
                    return;
                }
            };
            
            tracing::info!("Deleting S3 Object (Self-Described) -> Bucket: {}, Key: {}", bucket, key);
            
            //即使配置文件的 bucket 变了，我们也删除 Tiddler 中记录的那个 bucket 里的文件
            let _ = client.delete_object()
                .bucket(&bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| tracing::error!("Failed to delete S3 object: {}", e));
        }
        return;
    }

    let uri = match get_field("_canonical_uri") {
        Some(u) => u,
        None => return,
    };
    
    // === 分支 B: 明确标记为 Local 存储 ===
    if storage_type.as_deref() == Some("local") {
        // 本地存储逻辑（略，你可以像 put_tiddler 里那样存 _file_storage="local"）
        // ... (原有的本地文件删除逻辑) ...
        let filename = &uri["/files/".len()..];
        if filename.contains("..") || filename.contains('/') || filename.contains('\\') { return; }
        let file_path = config.files_dir.join(filename);
        let _ = fs::remove_file(&file_path).await;
        tracing::info!("Deleted local file (Self-Described): {:?}", file_path);
        return;
    }

    // === 分支 C: 兼容旧数据 (Legacy) ===
    // 如果没有 _file_storage 字段，回退到基于 _canonical_uri 解析的逻辑
    
    if uri.starts_with("/files/") {
        // ... (原有的本地文件删除逻辑) ...
        let filename = &uri["/files/".len()..];
        if filename.contains("..") || filename.contains('/') || filename.contains('\\') { return; }
        let file_path = config.files_dir.join(filename);
        let _ = fs::remove_file(&file_path).await;
        tracing::info!("Deleted local file (Legacy detection): {:?}", file_path);
    } 
    else if state.s3_client.is_some() && uri.starts_with(&state.public_url_base) {
        // ... (原有的 S3 删除逻辑，依赖 config.toml 中的 public_url_base) ...
        let client = state.s3_client.as_ref().unwrap();
        let mut key = &uri[state.public_url_base.len()..];
        if key.starts_with('/') { key = &key[1..]; }
        
        tracing::info!("Deleting S3 Object (Legacy URI match) -> Bucket: {}, Key: {}", state.bucket_name, key);
        
        let _ = client.delete_object()
            .bucket(&state.bucket_name)
            .key(key)
            .send()
            .await
            .map_err(|e| tracing::error!("Failed to delete S3 object: {}", e));
    }
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
                                obj.insert("_file_storage".to_string(), serde_json::Value::String("local".to_string()));
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

async fn add_inbox_item(
    Extension(ds): Extension<DataStore>,
    extract::Json(payload): extract::Json<InboxRequest>,
) -> AppResult<axum::Json<serde_json::Value>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;

    // --- A. 时间处理 ---
    // 尝试解析 ISO 8601 时间戳，如果失败则使用服务器当前时间
    // TiddlyWiki 需要 17 位时间格式: YYYYMMDDhhmmssXXX
    let created_dt = chrono::DateTime::parse_from_rfc3339(&payload.timestamp)
        .map(|dt| dt.with_timezone(&chrono::Local))
        .unwrap_or_else(|_| chrono::Local::now());
    let tw_timestamp = created_dt.format("%Y%m%d%H%M%S%3f").to_string();

    // --- B. 正文与 Context 处理 ---
    // 将 context 格式化为 Markdown 引用块并拼接到正文头部
    let final_text = match payload.context {
        Some(ctx) if !ctx.is_empty() => {
            format!("> **Context**: {}\n\n---\n\n{}", ctx, payload.text)
        }
        _ => payload.text,
    };

    // --- C. 标签处理 ---
    // 1. 强制包含 "Inbox"
    // 2. 如果标签包含空格，使用 [[ ]] 包裹
    let mut tags_list = payload.tags.clone();
    if !tags_list.iter().any(|t| t == "Inbox") {
        tags_list.push("Inbox".to_string());
    }
    
    let tags_string = tags_list.iter()
        .map(|s| {
            if s.contains(' ') {
                format!("[[{}]]", s)
            } else {
                s.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join(" ");

    // --- D. 构建 Tiddler 数据 ---
    let mut tiddler_map = serde_json::Map::new();

    // 基础字段
    tiddler_map.insert("title".to_string(), serde_json::Value::String(payload.title.clone()));
    tiddler_map.insert("text".to_string(), serde_json::Value::String(final_text));
    tiddler_map.insert("tags".to_string(), serde_json::Value::String(tags_string));
    tiddler_map.insert("created".to_string(), serde_json::Value::String(tw_timestamp.clone()));
    tiddler_map.insert("modified".to_string(), serde_json::Value::String(tw_timestamp.clone()));

    // 类型字段：
    // 1. type: 指定渲染器为 Markdown，适应 LLM 输出
    // 2. item_type: 存储业务类型 (observation, conclusion 等)
    tiddler_map.insert("type".to_string(), serde_json::Value::String("text/markdown".to_string()));
    tiddler_map.insert("item_type".to_string(), serde_json::Value::String(payload.item_type));

    // Metadata 展开字段
    // 将 metadata 里的 kv 展平放入 tiddler 字段中 (例如 child_age, priority 等)
    if let Some(meta) = payload.metadata {
        for (k, v) in meta {
            // 保护核心字段不被 metadata 覆盖
            if !["title", "text", "tags", "created", "modified", "type"].contains(&k.as_str()) {
                tiddler_map.insert(k, v);
            }
        }
    }

    let tiddler_json = serde_json::Value::Object(tiddler_map);

    // --- E. 存入数据库 ---
    // 复用 Tiddler::from_value 进行转换
    let tiddler = Tiddler::from_value(tiddler_json)?;
    tiddlers.put(tiddler)?;

    tracing::info!("📥 Inbox captured: '{}' [{}]", payload.title, tw_timestamp);

    Ok(axum::Json(serde_json::json!({
        "status": "ok",
        "title": payload.title,
        "created": tw_timestamp
    })))
}