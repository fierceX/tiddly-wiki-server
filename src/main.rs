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
use chrono::{Local, Utc};
use tower_http::set_header::SetResponseHeaderLayer; // 引入修改响应头的层
use clap::Parser;
use rusqlite::params;
use rusqlite::Connection;
use rusqlite::backup::Backup;
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
    #[serde(default)]
    backup: BackupConfig,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct BackupConfig {
    #[serde(default = "default_backup_enable")]
    enable: bool,
    #[serde(default = "default_backup_interval")]
    interval_hours: u64,
    #[serde(default = "default_backup_retention")]
    retention_count: usize,
    #[serde(default = "default_backup_s3_bucket")]
    s3_bucket: String,
    #[serde(default = "default_backup_s3_prefix")]
    s3_prefix: String,
}

fn default_backup_enable() -> bool { false }
fn default_backup_interval() -> u64 { 24 }
fn default_backup_retention() -> usize { 7 }
fn default_backup_s3_bucket() -> String { String::new() }
fn default_backup_s3_prefix() -> String { "backups/".to_string() }

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
    
    #[serde(default)]
    #[serde(rename = "content")] // "content" 字段（原名）
    content: Option<String>,
    
    #[serde(default)]
    text: Option<String>, // "text" 字段（别名，Agent 常用此名）
    
    timestamp: String, // ISO 8601 格式字符串
    
    #[serde(default)]
    context: Option<String>,
    
    #[serde(default)]
    metadata: Option<HashMap<String, serde_json::Value>>,
}

// --- Agent 友好 API 的请求参数 ---

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,              // 搜索关键词（匹配 title + text）
    tag: Option<String>,            // 按标签过滤
    item_type: Option<String>,      // 按 item_type 字段过滤
    mode: Option<String>,           // 搜索模式：fts(默认) | regex
    include_text: Option<bool>,     // 是否返回全文（默认 false）
    limit: Option<usize>,           // 每页条数（默认 20）
    offset: Option<usize>,          // 偏移（默认 0）
    // 时间范围过滤（TiddlyWiki 时间格式: YYYYMMDDHHMMSSmmm）
    modified_after: Option<String>,   // 修改时间 ≥
    modified_before: Option<String>,  // 修改时间 ≤
    created_after: Option<String>,    // 创建时间 ≥
    created_before: Option<String>,   // 创建时间 ≤
}

#[derive(Deserialize)]
struct TitleQuery {
    title: String,
}

#[derive(Deserialize)]
struct MemoryContextParams {
    q: Option<String>,
    limit: Option<usize>,
    hours: Option<u64>,
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

    // 3a. 自动恢复：本地无数据库时从 S3 备份拉取
    if !config.server.db_path.exists() {
        if let Some(ref restore_cfg) = try_get_restore_config(&config.s3, &config.backup) {
            tracing::info!("Local database not found, attempting restore from S3 backup...");
            if let Err(e) = try_restore_from_s3(restore_cfg, &config.server.db_path).await {
                tracing::warn!("Restore from S3 failed (server will create new database): {}", e);
            }
        }
    }

    // 3b. 初始化数据库
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

    // 5b. 后台自动备份任务
    if config.backup.enable {
        let server_cfg = config.server.clone();
        let backup_cfg = config.backup.clone();
        let state = app_state.clone();
        tokio::spawn(async move {
            run_auto_backup(server_cfg, backup_cfg, state).await;
        });
    }

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
        .route("/api", get(api_index))
        .route("/api/sign-upload", get(get_presigned_url))
        .route("/api/tags", get(all_tags))
        .route("/api/search", get(search_tiddlers))
        .route("/api/memory/context", get(memory_context))
        .route("/api/tiddlers", get(get_tiddler_by_query))
        .route("/api/tiddlers/tag/{tag}", get(tiddlers_by_tag))
        .route("/api/tiddlers/{title}/links", get(tiddler_links))
        .route("/api/tiddlers/{title}/backlinks", get(tiddler_backlinks))
        .route("/api/inbox", get(list_inbox).post(add_inbox_item))
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

// ─── 自动恢复 ───────────────────────────────────────────────

struct RestoreConfig {
    access_key: String,
    secret_key: String,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
}

fn try_get_restore_config(s3: &S3Config, backup: &BackupConfig) -> Option<RestoreConfig> {
    if !s3.enable || backup.s3_bucket.is_empty() {
        return None;
    }
    Some(RestoreConfig {
        access_key: s3.access_key.clone(),
        secret_key: s3.secret_key.clone(),
        endpoint: s3.endpoint.clone(),
        region: s3.region.clone(),
        bucket: backup.s3_bucket.clone(),
        prefix: backup.s3_prefix.clone(),
    })
}

async fn try_restore_from_s3(cfg: &RestoreConfig, db_path: &std::path::Path) -> Result<(), String> {
    let credentials = Credentials::new(&cfg.access_key, &cfg.secret_key, None, None, "restore");
    let region = Region::new(cfg.region.clone());
    let s3_config = aws_config::defaults(BehaviorVersion::latest())
        .region(region)
        .credentials_provider(credentials)
        .endpoint_url(&cfg.endpoint)
        .load()
        .await;
    let client = aws_sdk_s3::Client::new(&s3_config);

    // 列出备份目录下所有 sqlite_*.db 文件，取最新的
    let resp = client.list_objects_v2()
        .bucket(&cfg.bucket)
        .prefix(&cfg.prefix)
        .send()
        .await
        .map_err(|e| format!("list backup objects: {}", e))?;

    let backups: Vec<&aws_sdk_s3::types::Object> = resp.contents().iter()
        .filter(|o| o.key().map_or(false, |k| k.contains("sqlite_") && k.ends_with(".db")))
        .collect();

    let latest = backups.iter()
        .max_by_key(|o| o.last_modified())
        .ok_or_else(|| "no backup found on S3".to_string())?;

    let key = latest.key().ok_or_else(|| "backup object key missing".to_string())?;
    tracing::info!("Downloading latest backup: {}", key);

    // 确保父目录存在
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create db parent dir: {}", e))?;
    }

    let resp = client.get_object()
        .bucket(&cfg.bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| format!("download backup: {}", e))?;

    let body = resp.body;
    let data = body.collect().await
        .map_err(|e| format!("download backup: {}", e))?;
    let bytes = data.into_bytes();
    tokio::fs::write(db_path, &bytes).await
        .map_err(|e| format!("write db file: {}", e))?;

    tracing::info!("Restored database from S3 backup: {}", key);
    Ok(())
}

// ─── 后台自动备份 ───────────────────────────────────────────

async fn run_auto_backup(server: ServerConfig, cfg: BackupConfig, state: Arc<AppState>) {
    // 等待服务器稳定
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;

    if state.s3_client.is_none() {
        tracing::warn!("Auto-backup: S3 client not available, backup disabled");
        return;
    }
    let client = state.s3_client.as_ref().unwrap();
    let bucket = &cfg.s3_bucket;
    if bucket.is_empty() {
        tracing::warn!("Auto-backup: s3_bucket is empty, backup disabled");
        return;
    }

    tracing::info!("Auto-backup started (interval: {}h, retention: {})", cfg.interval_hours, cfg.retention_count);

    // 首次备份：检查 S3 上是否有已有备份
    let has_backup = check_existing_backup(client, bucket, &cfg.s3_prefix).await;
    if !has_backup {
        tracing::info!("Auto-backup: no existing backup found, performing initial backup");
        if let Err(e) = do_backup_and_upload(client, bucket, &cfg.s3_prefix, &server.db_path).await {
            tracing::error!("Auto-backup: initial backup failed: {}", e);
        }
    } else {
        tracing::info!("Auto-backup: found existing backup on S3, entering monitoring loop");
    }

    let interval_dur = std::time::Duration::from_secs(cfg.interval_hours * 3600);

    loop {
        tokio::time::sleep(interval_dur).await;

        // 检查是否有变更
        let changed = match check_for_changes(client, bucket, &cfg.s3_prefix, &server.db_path).await {
            Ok(true) => true,
            Ok(false) => {
                tracing::info!("Auto-backup: no changes detected, skipping");
                continue;
            }
            Err(e) => {
                tracing::warn!("Auto-backup: change detection failed (will still backup): {}", e);
                true
            }
        };

        if !changed {
            continue;
        }

        tracing::info!("Auto-backup: changes detected, starting backup");
        if let Err(e) = do_backup_and_upload(client, bucket, &cfg.s3_prefix, &server.db_path).await {
            tracing::error!("Auto-backup: backup failed: {}", e);
            continue;
        }

        // 清理旧备份（保留最近 N 个）
        if let Err(e) = rotate_backups(client, bucket, &cfg.s3_prefix, cfg.retention_count).await {
            tracing::warn!("Auto-backup: rotation failed: {}", e);
        }
    }
}

/// 检查 S3 上是否已有备份文件
async fn check_existing_backup(client: &aws_sdk_s3::Client, bucket: &str, prefix: &str) -> bool {
    let resp = client.list_objects_v2().bucket(bucket).prefix(prefix).max_keys(1).send().await;
    match resp {
        Ok(r) => !r.contents().is_empty(),
        Err(e) => {
            tracing::warn!("Auto-backup: cannot list S3 objects: {}", e);
            false
        }
    }
}

/// 检查数据库是否有变更（比较当前 max modified 与上次备份记录）
async fn check_for_changes(client: &aws_sdk_s3::Client, bucket: &str, prefix: &str, db_path: &std::path::Path) -> Result<bool, String> {
    // 1. 从 S3 获取上次备份的 max_modified
    let marker_key = format!("{}_checkpoint.txt", prefix.trim_end_matches('/'));
    let last_max_modified = match client.get_object().bucket(bucket).key(&marker_key).send().await {
        Ok(resp) => {
            let bytes = resp.body.collect().await.map_err(|e| format!("read checkpoint: {}", e))?;
            String::from_utf8_lossy(&bytes.into_bytes()).trim().to_string()
        }
        Err(_) => String::new(), // 无 checkpoint → 需要备份
    };
    // 2. 查询数据库当前 max modified（直接用 SQL 聚合，避免加载全量 meta）
    let conn = rusqlite::Connection::open(db_path).map_err(|e| format!("open db: {}", e))?;
    let current_max: String = conn.query_row(
        "SELECT IFNULL(MAX(json_extract(meta, '$.modified')), '') FROM tiddlers",
        [],
        |r| r.get(0),
    ).map_err(|e| format!("query max modified: {}", e))?;

    Ok(!current_max.is_empty() && current_max != last_max_modified)
}

/// 执行备份并上传到 S3
async fn do_backup_and_upload(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
    db_path: &std::path::Path,
) -> Result<(), String> {
    use aws_sdk_s3::primitives::ByteStream;
    // 1. 备份到临时文件（嵌套块确保 backup 借用在 drop 前结束）
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!("tiddlywiki_backup_{}.db", timestamp));

    {
        let src = Connection::open(db_path).map_err(|e| format!("open source db: {}", e))?;
        let mut dst = Connection::open(&tmp_path).map_err(|e| format!("create temp db: {}", e))?;
        let backup = Backup::new(&src, &mut dst).map_err(|e| format!("backup init: {}", e))?;
        backup.run_to_completion(100, std::time::Duration::from_millis(250), None)
            .map_err(|e| format!("backup run: {}", e))?;
    }

    // 2. 获取当前最大 modified 时间戳（直接用 SQL 聚合，避免加载全量 meta）
    let max_modified: String = {
        let conn = Connection::open(db_path).map_err(|e| format!("open db for meta: {}", e))?;
        conn.query_row(
            "SELECT IFNULL(MAX(json_extract(meta, '$.modified')), '') FROM tiddlers",
            [],
            |r| r.get(0),
        ).map_err(|e| format!("query max modified: {}", e))?
    };

    // 3. 上传数据库备份到 S3
    let s3_key = format!("{}sqlite_{}.db", prefix, timestamp);
    let body = ByteStream::from_path(&tmp_path).await
        .map_err(|e| format!("read temp file: {}", e))?;

    client.put_object()
        .bucket(bucket)
        .key(&s3_key)
        .body(body)
        .send()
        .await
        .map_err(|e| format!("S3 upload failed: {}", e))?;

    tracing::info!("Auto-backup: uploaded to s3://{}/{}", bucket, s3_key);

    // 4. 上传 checkpoint（记录 max_modified 供变更检测）
    if !max_modified.is_empty() {
        let marker_key = format!("{}_checkpoint.txt", prefix.trim_end_matches('/'));
        client.put_object()
            .bucket(bucket)
            .key(&marker_key)
            .body(ByteStream::from(max_modified.clone().into_bytes()))
            .send()
            .await
            .map_err(|e| format!("S3 upload checkpoint failed: {}", e))?;
    }

    // 5. 清理临时文件
    let _ = std::fs::remove_file(&tmp_path);
    Ok(())
}

/// 轮转删除旧备份，只保留最近 retention_count 个
async fn rotate_backups(client: &aws_sdk_s3::Client, bucket: &str, prefix: &str, keep: usize) -> Result<(), String> {
    // 列出所有 backup 对象
    let resp = client.list_objects_v2().bucket(bucket).prefix(prefix).send().await
        .map_err(|e| format!("list objects: {}", e))?;

    let mut objects: Vec<String> = resp.contents().iter()
        .filter_map(|obj| {
            let key = obj.key()?;
            // 只匹配 sqlite_*.db 文件
            if key.contains("sqlite_") && key.ends_with(".db") {
                Some(key.to_string())
            } else {
                None
            }
        })
        .collect();

    if objects.len() <= keep {
        return Ok(()); // 不需要清理
    }

    // 按 key 排序（命名含时间戳 YYYYMMDD_HHMMSS，字典序=时间序）
    objects.sort();

    // 删除最旧的 (len - keep) 个
    let to_delete = objects.len() - keep;
    for key in objects.iter().take(to_delete) {
        client.delete_object().bucket(bucket).key(key).send().await
            .map_err(|e| format!("delete {} failed: {}", key, e))?;
        tracing::debug!("Auto-backup: deleted old backup {}", key);
    }

    tracing::info!("Auto-backup: rotation complete (deleted {}, kept {})", to_delete, keep);
    Ok(())
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

    // 初始化 jieba 分词（每次启动加载一次）
    let jieba = jieba_rs::Jieba::new();

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
        
        // 执行初始化 SQL 脚本（含 FTS5 虚拟表）
        let init_script = include_str!("./init.sql");
        
        cxn.execute_batch(init_script)
            .map_err(|e| AppError::Database(format!("初始化数据库失败: {}", e)))?;
        insert_default_data(S3_PLUGIN_JSON,&cxn)?;
        insert_default_data(CPL_PLUGIN_JSON,&cxn)?;
        
        tracing::info!("The database initialization has been completed.")
    } else {
        tracing::info!("Use the existing database!");
        
        // FTS5 迁移：重建索引（drop 旧触发器/表 → 重建 → 用 jieba 分词回填数据）
        let _ = cxn.execute_batch(r#"
            DROP TRIGGER IF EXISTS tiddlers_fts_ai;
            DROP TRIGGER IF EXISTS tiddlers_fts_ad;
            DROP TRIGGER IF EXISTS tiddlers_fts_au;
            DROP TABLE IF EXISTS tiddlers_fts;
        "#);
        let fts_migration = include_str!("./init.sql");
        if let Err(e) = cxn.execute_batch(fts_migration) {
            tracing::warn!("FTS5 migration failed: {}", e);
        } else {
            // 用 jieba 分词后逐条回填 FTS5 索引
            let rebuild_result = (|| -> Result<(), rusqlite::Error> {
                let mut stmt = cxn.prepare("SELECT rowid, meta FROM tiddlers")?;
                let rows: Vec<(i64, serde_json::Value)> = stmt.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?.collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                let mut ins = cxn.prepare(
                    "INSERT INTO tiddlers_fts(rowid, title, text, tags) VALUES (?1, ?2, ?3, ?4)"
                )?;
                for (rowid, meta) in rows {
                    let title = meta.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let text = meta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    let tags = meta.get("tags").and_then(|v| v.as_str()).unwrap_or("");
                    let tok_title = jieba.cut(title, true).join(" ");
                    let tok_text = jieba.cut(text, true).join(" ");
                    ins.execute(rusqlite::params![rowid, tok_title, tok_text, tags])?;
                }
                Ok(())
            })();
            match rebuild_result {
                Ok(()) => tracing::info!("FTS5 full-text index ready (jieba tokenized)."),
                Err(e) => tracing::warn!("FTS5 index rebuild warning: {}", e),
            }
        }
    }

    // [[链接]] 索引回填
    {
        let count: i64 = cxn.query_row(
            "SELECT COUNT(*) FROM tiddler_links",
            [],
            |r| r.get(0),
        ).unwrap_or(0);
        let has_dirty: bool = cxn.query_row(
            "SELECT COUNT(*) > 0 FROM tiddler_links WHERE target LIKE '%|%'",
            [],
            |r| r.get(0),
        ).unwrap_or(false);

        let needs_backfill = count == 0 || has_dirty;

        if has_dirty {
            tracing::warn!("Found dirty link data (targets containing '|'), clearing and re-backfilling...");
            if let Err(e) = cxn.execute("DELETE FROM tiddler_links", []) {
                tracing::warn!("failed to clear tiddler_links: {}", e);
            }
        }

        if needs_backfill {
            tracing::info!("tiddler_links table is empty, backfilling link index...");
            let mut stmt = cxn.prepare("SELECT title, meta FROM tiddlers").unwrap_or_else(|e| {
                tracing::warn!("cannot prepare link backfill query: {}", e);
                // 返回一个空语句占位，实际上跳过
                return cxn.prepare("SELECT '', '{}' FROM tiddlers WHERE 1=0").unwrap();
            });
            if let Ok(rows) = stmt.query_map([], |r| {
                let title: String = r.get(0)?;
                let meta_val: String = r.get(1)?;
                Ok((title, meta_val))
            }) {
                let link_re = regex::Regex::new(r"\[\[([^\]]+?)(?:\|([^\]]+))?\]\]").unwrap();
                let mut inserted = 0usize;
                let mut failed = 0usize;
                for row in rows.flatten() {
                    let (title, meta_str) = row;
                    // 从 meta JSON 中提取 text 字段
                    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_str) {
                        let text = meta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        for cap in link_re.captures_iter(text) {
                            let target = cap.get(2).or_else(|| cap.get(1)).map(|m| m.as_str()).unwrap_or("");
                            if target.is_empty() { continue; }
                            if let Err(e) = cxn.execute(
                                "INSERT OR IGNORE INTO tiddler_links(source, target) VALUES (?1, ?2)",
                                rusqlite::params![title, target],
                            ) {
                                tracing::warn!("link backfill failed: {} -> {}: {}", title, target, e);
                                failed += 1;
                            } else {
                                inserted += 1;
                            }
                        }
                    }
                }
                tracing::info!("Link index backfill complete: {} links indexed, {} failed", inserted, failed);
            }
        } else {
            tracing::debug!("tiddler_links has {} rows, skipping backfill", count);
        }
    }

    let tiddlers = Tiddlers { cxn, jieba };
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

    // 流式写入：逐条序列化，避免中间 Vec<Value> + 完整 JSON 字符串的双倍内存
    let mut buffer = Vec::with_capacity(
        template.prefix.len() + template.suffix.len() + 8192
    );
    buffer.extend(template.prefix.as_bytes());
    buffer.push(b',');

    for (i, t) in tiddlers.iter().enumerate() {
        if i > 0 { buffer.push(b','); }
        let json = serde_json::to_string(&t.as_value())
            .map_err(|e| AppError::Serialization(format!("error serializing db: {}", e)))?;
        // 在序列化后的字符串中直接转义 </script>，避免 replace 整份复制
        buffer.extend(json.replace("</script>", "<\\/script>").as_bytes());
    }

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
    Extension(auth): Extension<Option<AuthConfig>>,
    extract::Path(title): extract::Path<String>,
    extract::Json(mut v): extract::Json<serde_json::Value>,
) -> AppResult<axum::http::Response<String>> {
    use axum::http::response::Response;

    let default_username = auth.as_ref()
        .map(|a| a.username.clone())
        .unwrap_or_else(|| "anonymous".to_string());

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

    let now = Local::now().format("%Y%m%d%H%M%S%3f").to_string();

    if let Some(old_tiddler) = tiddlers.pop(&title)? {
        // ── 更新已有条目 ──
        new_tiddler.revision += 1;
        if let Some(obj) = new_tiddler.meta.as_object_mut() {
            // 保留原有的 created/creator（客户端没发时）
            if !obj.contains_key("created") {
                if let Some(old_v) = old_tiddler.meta.get("created") {
                    obj.insert("created".to_string(), old_v.clone());
                } else {
                    obj.insert("created".to_string(), Value::String(now.clone()));
                }
            }
            if !obj.contains_key("creator") {
                if let Some(old_v) = old_tiddler.meta.get("creator") {
                    obj.insert("creator".to_string(), old_v.clone());
                }
            }
            // 更新 modified/modifier
            if !obj.contains_key("modified") {
                obj.insert("modified".to_string(), Value::String(now.clone()));
            }
            if !obj.contains_key("modifier") {
                if let Some(old_v) = old_tiddler.meta.get("modifier") {
                    obj.insert("modifier".to_string(), old_v.clone());
                }
            }
        }
    } else {
        // ── 新建条目 ──
        if let Some(obj) = new_tiddler.meta.as_object_mut() {
            if !obj.contains_key("created") {
                obj.insert("created".to_string(), Value::String(now.clone()));
            }
            if !obj.contains_key("modified") {
                obj.insert("modified".to_string(), Value::String(now.clone()));
            }
            if !obj.contains_key("creator") {
                obj.insert("creator".to_string(), Value::String(default_username.clone()));
            }
            if !obj.contains_key("modifier") {
                obj.insert("modifier".to_string(), Value::String(default_username.clone()));
            }
        }
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
    jieba: jieba_rs::Jieba,
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

    /// 按标签过滤查询（使用 SQL LIKE，避免全量加载后再过滤）
    pub(crate) fn get_all_by_tag(&self, tag: &str) -> AppResult<Vec<Tiddler>> {
        tracing::debug!("getting tiddlers by tag: {}", tag);
        // 在 meta 的 tags 字段中查找，LIKE 匹配空格分隔的标签
        let pattern = format!("%{}%", tag);
        const Q: &str = r#"SELECT title, revision, meta FROM tiddlers WHERE meta LIKE ?1"#;
        let mut stmt = self.cxn.prepare_cached(Q).map_err(AppError::from)?;
        let raw_tiddlers = stmt
            .query_map([&pattern], |r| r.get::<usize, serde_json::Value>(2))
            .map_err(AppError::from)?;
        let mut tiddlers = Vec::new();
        for qt in raw_tiddlers {
            let raw = qt.map_err(AppError::from)?;
            tiddlers.push(Tiddler::from_value(raw)?);
        }
        Ok(tiddlers)
    }

    pub(crate) fn put(&mut self, tiddler: Tiddler) -> AppResult<()> {
        tracing::debug!("putting tiddler: {}", tiddler.title);
        let text = tiddler.get_text_field().unwrap_or("").to_string();
        let tags = tiddler.meta.get("tags").and_then(|v| v.as_str()).unwrap_or("");
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
        // Best-effort FTS 同步
        self.sync_fts_insert(&tiddler.title, &text, tags);
        // 链接索引同步
        self.sync_links(&tiddler.title, &text);
        Ok(())
    }

    pub(crate) fn pop(&mut self, title: &str) -> AppResult<Option<Tiddler>> {
        tracing::debug!("popping tiddler: {}", title);
        let result = self.get(title)?;
        // ★ 在删除主表记录前捕获 rowid，供后续 FTS 清理使用
        let rowid: Option<i64> = result.as_ref().and_then(|_| {
            self.cxn.query_row(
                "SELECT rowid FROM tiddlers WHERE title = ?1",
                rusqlite::params![title],
                |r| r.get(0),
            ).ok()
        });
        const DELETE: &str = "DELETE FROM tiddlers WHERE title = :title";
        let mut stmt = self.cxn.prepare(DELETE).map_err(|e| AppError::Database(format!("Error preparing {}: {}", DELETE, e)))?;
        stmt.execute(rusqlite::named_params! { ":title": title })
            .map_err(|e| AppError::Database(format!("Error removing tiddler: {}", e)))?;
        if let Some(rid) = rowid {
            self.sync_fts_delete_by_rowid(rid, title);
        }
        // 链接索引清理
        self.sync_links_delete(title);
        Ok(result)
    }

    /// FTS5 插入同步（best-effort：失败只记日志，不回滚主写入）
    /// 使用 jieba-rs 对中文文本分词后存入 FTS5，使 unicode61 tokenizer 能正确索引中文。
    fn sync_fts_insert(&self, title: &str, text: &str, tags: &str) {
        let tokenized_title = self.jieba_tokenize(title);
        let tokenized_text = self.jieba_tokenize(text);
        // tags 本身是空格分隔的，unicode61 已可正确处理，无需额外分词
        if let Err(e) = self.cxn.execute(
            "INSERT OR REPLACE INTO tiddlers_fts(rowid, title, text, tags) VALUES (last_insert_rowid(), ?1, ?2, ?3)",
            rusqlite::params![tokenized_title, tokenized_text, tags],
        ) {
            tracing::warn!("FTS sync insert failed for '{}': {}", title, e);
        }
    }

    /// 对文本调用 jieba 分词，空格连接（用于 FTS5 索引/查询）
    fn jieba_tokenize(&self, input: &str) -> String {
        let words = self.jieba.cut(input, true);
        words.join(" ")
    }

    /// FTS5 删除同步（best-effort：失败只记日志，不回滚主写入）
    /// 对于无 content= 的 FTS5 表，直接用 DELETE 清理即可，无需 'delete' INSERT 语法。
    fn sync_fts_delete_by_rowid(&self, rowid: i64, title: &str) {
        if let Err(e) = self.cxn.execute(
            "DELETE FROM tiddlers_fts WHERE rowid = ?1",
            rusqlite::params![rowid],
        ) {
            tracing::warn!("FTS sync delete failed for '{}': {}", title, e);
        }
    }

    /// 链接索引同步：解析 `[[链接]]` 并写入 tiddler_links 表（best-effort）
    fn sync_links(&self, source: &str, text: &str) {
        // 1. 清除该来源的旧链接记录
        if let Err(e) = self.cxn.execute(
            "DELETE FROM tiddler_links WHERE source = ?1",
            rusqlite::params![source],
        ) {
            tracing::warn!("link sync delete failed for '{}': {}", source, e);
            return;
        }
        // 2. 解析 [[链接]] 并批量插入（编译一次 regex 供重复调用）
        //    TiddlyWiki 语法：[[目标标题]] 或 [[显示名|目标标题]]
        static LINK_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let link_re = LINK_RE.get_or_init(|| {
            // 匹配 [[目标]] 或 [[显示名|目标]]，提取最后一个 pipe 后的部分为目标
            regex::Regex::new(r"\[\[([^\]]+?)(?:\|([^\]]+))?\]\]").expect("invalid link regex")
        });
        let mut inserted = 0;
        for cap in link_re.captures_iter(text) {
            // cap[1] = [[ 和 | 之间的部分（或没有 | 时的全部内容）
            // cap[2] = | 和 ]] 之间的部分（如果有 |）
            let target = cap.get(2).or_else(|| cap.get(1)).map(|m| m.as_str()).unwrap_or("");
            if target.is_empty() { continue; }
            if let Err(e) = self.cxn.execute(
                "INSERT OR IGNORE INTO tiddler_links(source, target) VALUES (?1, ?2)",
                rusqlite::params![source, target],
            ) {
                tracing::warn!("link sync insert failed for '{}' -> '{}': {}", source, target, e);
            } else {
                inserted += 1;
            }
        }
        if inserted > 0 {
            tracing::debug!("synced {} links from '{}'", inserted, source);
        }
    }

    /// 链接索引清理：删除某个来源的所有链接记录
    fn sync_links_delete(&self, source: &str) {
        if let Err(e) = self.cxn.execute(
            "DELETE FROM tiddler_links WHERE source = ?1",
            rusqlite::params![source],
        ) {
            tracing::warn!("link sync delete failed for '{}': {}", source, e);
        }
    }

    /// FTS5 全文搜索——用 jieba 分词生成 FTS5 AND 查询
    pub(crate) fn search_fts(&self, query: &str, limit: usize, offset: usize) -> AppResult<Vec<Tiddler>> {
        let fts_query = self.build_fts_query(query);
        const SEARCH: &str = r#"
            SELECT t.meta FROM tiddlers t
            JOIN tiddlers_fts fts ON t.rowid = fts.rowid
            WHERE tiddlers_fts MATCH ?
            ORDER BY rank
            LIMIT ? OFFSET ?
        "#;
        let mut stmt = self.cxn.prepare_cached(SEARCH).map_err(AppError::from)?;
        let results = stmt.query_map(
            rusqlite::params![fts_query, limit, offset],
            |r| r.get::<usize, serde_json::Value>(0),
        ).map_err(AppError::from)?;

        results
            .map(|r| Tiddler::from_value(r.map_err(AppError::from)?))
            .collect()
    }

    /// 将用户查询转换为 FTS5 查询字符串：jieba 分词 + 前缀通配符（AND 语义）
    /// "天气" → jieba → ["天气"] → "天气*" → FTS5 前缀匹配 "天气晴朗"
    fn build_fts_query(&self, raw: &str) -> String {
        let tokens = self.jieba.cut(raw, true);
        tokens.iter()
            .map(|t| format!("{}*", t))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 正向链接：某条目链接了哪些目标
    pub(crate) fn links(&self, title: &str, limit: usize, offset: usize) -> AppResult<Vec<String>> {
        // SQLite: LIMIT -1 表示无限制
        let sql_limit = if limit > 0 { limit as i64 } else { -1 };
        const Q: &str = "SELECT target FROM tiddler_links WHERE source = ?1 ORDER BY target LIMIT ?2 OFFSET ?3";
        let mut stmt = self.cxn.prepare_cached(Q)?;
        let results = stmt.query_map(
            rusqlite::params![title, sql_limit, offset],
            |r| r.get::<usize, String>(0),
        )?;
        let mut out = Vec::new();
        for r in results {
            out.push(r.map_err(AppError::from)?);
        }
        Ok(out)
    }

    /// 反向链接：哪些条目链接到某目标
    pub(crate) fn backlinks(&self, title: &str, limit: usize, offset: usize) -> AppResult<Vec<String>> {
        let sql_limit = if limit > 0 { limit as i64 } else { -1 };
        const Q: &str = "SELECT source FROM tiddler_links WHERE target = ?1 ORDER BY source LIMIT ?2 OFFSET ?3";
        let mut stmt = self.cxn.prepare_cached(Q)?;
        let results = stmt.query_map(
            rusqlite::params![title, sql_limit, offset],
            |r| r.get::<usize, String>(0),
        )?;
        let mut out = Vec::new();
        for r in results {
            out.push(r.map_err(AppError::from)?);
        }
        Ok(out)
    }

    /// 获取所有标签及其出现次数（按次数降序排列）
    pub(crate) fn tags(&self) -> AppResult<Vec<(String, usize)>> {
        use std::collections::BTreeMap;
        // 只加载 meta，不构建 Tiddler 结构体，避免 all() 的额外开销
        let mut stmt = self.cxn.prepare_cached("SELECT meta FROM tiddlers")
            .map_err(AppError::from)?;
        let rows = stmt.query_map([], |r| r.get::<usize, serde_json::Value>(0))
            .map_err(AppError::from)?;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in rows {
            let meta = row.map_err(AppError::from)?;
            if let Some(tags_str) = meta.get("tags").and_then(|v| v.as_str()) {
                for tag in tags_str.split(' ').filter(|s| !s.is_empty()) {
                    *counts.entry(tag.to_string()).or_default() += 1;
                }
            }
        }
        let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(sorted)
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

    /// 获取 meta JSON 中的 text 字段
    pub(crate) fn get_text_field(&self) -> Option<&str> {
        self.meta.get("text").and_then(|v| v.as_str())
    }

    /// 检查是否有指定标签（支持空格分隔和 [[ ]] 包裹格式）
    pub(crate) fn has_tag(&self, tag: &str) -> bool {
        self.meta.get("tags").and_then(|v| v.as_str())
            .map(|tags_str| {
                tags_str.split_whitespace()
                    .any(|t| t == tag || t == &format!("[[{}]]", tag))
            })
            .unwrap_or(false)
    }

    /// 获取指定字段值（优先从 fields 子对象查找，fallback 到顶层）
    pub(crate) fn get_field(&self, key: &str) -> Option<String> {
        self.meta.get("fields")
            .and_then(|f| f.get(key))
            .and_then(|v| v.as_str())
            .or_else(|| self.meta.get(key).and_then(|v| v.as_str()))
            .map(|s| s.to_string())
    }

    /// 检查条目的时间字段是否在指定范围内
    /// TiddlyWiki 时间格式 YYYYMMDDHHMMSSmmm 可直接字符串比较
    pub(crate) fn matches_time(
        &self,
        field: &str,            // "created" 或 "modified"
        after: Option<&str>,    // 时间 ≥ 此值
        before: Option<&str>,   // 时间 ≤ 此值
    ) -> bool {
        let val = match self.get_field(field) {
            Some(v) => v,
            None => return after.is_none() && before.is_none(),
        };
        if let Some(a) = after {
            if val.as_str() < a { return false; }
        }
        if let Some(b) = before {
            if val.as_str() > b { return false; }
        }
        true
    }

    /// Agent 友好的规范化输出格式
    /// - tags: 统一为数组
    /// - revision: 数字类型
    /// - 时间: ISO 8601
    /// - fields 展平
    pub(crate) fn as_agent_value(&self) -> Value {
        let mut map = serde_json::Map::new();

        map.insert("title".into(), Value::String(self.title.clone()));
        map.insert("revision".into(), Value::Number(self.revision.into()));

        // 展平 fields 子对象
        let flat = self.flatten_fields();

        // text
        if let Some(text) = flat.get("text").and_then(|v| v.as_str()) {
            map.insert("text".into(), Value::String(text.to_string()));
        }

        // tags: 统一为数组
        if let Some(tags_str) = flat.get("tags").and_then(|v| v.as_str()) {
            let tags: Vec<Value> = parse_tags(tags_str)
                .into_iter()
                .map(Value::String)
                .collect();
            map.insert("tags".into(), Value::Array(tags));
        }

        // 时间：统一为 ISO 8601
        for time_field in &["created", "modified"] {
            if let Some(ts) = flat.get(*time_field).and_then(|v| v.as_str()) {
                if let Some(iso) = tw_to_iso8601(ts) {
                    map.insert(time_field.to_string(), Value::String(iso));
                }
            }
        }

        // item_type
        if let Some(it) = flat.get("item_type").and_then(|v| v.as_str()) {
            map.insert("item_type".into(), Value::String(it.to_string()));
        }

        // type (MIME)
        if let Some(t) = flat.get("type").and_then(|v| v.as_str()) {
            map.insert("type".into(), Value::String(t.to_string()));
        }

        Value::Object(map)
    }

    fn flatten_fields(&self) -> serde_json::Map<String, Value> {
        let mut map = match self.meta.clone() {
            Value::Object(m) => m,
            _ => return serde_json::Map::new(),
        };
        if let Some(Value::Object(fields)) = map.remove("fields") {
            for (k, v) in fields {
                map.entry(k).or_insert(v);
            }
        }
        map
    }
}

/// 解析 TiddlyWiki 标签字符串 "tag1 tag2 [[multi word tag]]"
fn parse_tags(tags_str: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut current = String::new();
    let mut in_bracket = false;
    for ch in tags_str.chars() {
        match ch {
            '[' if !in_bracket => in_bracket = true,
            ']' if in_bracket => in_bracket = false,
            ' ' if !in_bracket => {
                if !current.is_empty() {
                    tags.push(std::mem::take(&mut current));
                }
            }
            c if c != '[' && c != ']' => current.push(c),
            _ => {}
        }
    }
    if !current.is_empty() {
        tags.push(current);
    }
    tags
}

/// TW 17 位时间戳 → ISO 8601
fn tw_to_iso8601(ts: &str) -> Option<String> {
    if ts.len() != 17 { return None; }
    let y: i32 = ts[0..4].parse().ok()?;
    let m: u32 = ts[4..6].parse().ok()?;
    let d: u32 = ts[6..8].parse().ok()?;
    let h: u32 = ts[8..10].parse().ok()?;
    let min: u32 = ts[10..12].parse().ok()?;
    let s: u32 = ts[12..14].parse().ok()?;
    let ms: u32 = ts[14..17].parse().ok()?;
    Some(format!("{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, h, min, s, ms))
}

// -----------------------------------------------------------------------------------

async fn status(Extension(status_config): Extension<Arc<Status>>) -> axum::Json<Status> {
    // axum::Json(STATUS)
    axum::Json(status_config.as_ref().clone())
}

// === Agent 友好 API Handlers ===

/// GET /api/search?q=关键词&tag=Inbox&item_type=note&include_text=true&limit=20&offset=0
async fn search_tiddlers(
    Extension(ds): Extension<DataStore>,
    extract::Query(params): extract::Query<SearchParams>,
) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;

    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);

    // 1. 获取候选集
    let candidates: Vec<Tiddler> = match params.mode.as_deref() {
        Some("regex") => {
            // regex 模式：全量加载 + 正则匹配 title 和 text
            let q = match params.q.as_deref() {
                Some(q) if !q.is_empty() => q,
                _ => return Ok(axum::Json(Vec::new())),
            };
            let pattern = match regex::Regex::new(q) {
                Ok(re) => re,
                Err(e) => {
                    tracing::warn!("Invalid regex pattern: {}", e);
                    return Ok(axum::Json(Vec::new()));
                }
            };
            let all = tiddlers.all()?;
            all.into_iter().filter(|t| {
                pattern.is_match(&t.title)
                    || t.get_text_field().map_or(false, |txt| pattern.is_match(txt))
            }).collect()
        }
        _ => {
            // fts 模式（默认）：FTS5（jieba 分词），FTS 失败时 fallback 全量
            if params.q.is_some() {
                let q = params.q.as_ref().unwrap();
                match tiddlers.search_fts(q, if limit > 0 { limit * 5 + offset } else { usize::MAX }, 0) {
                    Ok(fts_results) => fts_results,
                    Err(e) => {
                        tracing::warn!("FTS search failed, falling back to full scan: {:?}", e);
                        tiddlers.all()?
                    }
                }
            } else {
                tiddlers.all()?
            }
        }
    };

    // 2. 内存过滤（tag / item_type / 时间范围）
    let mut results: Vec<&Tiddler> = candidates.iter().filter(|t| {
        if let Some(ref tag) = params.tag {
            if !t.has_tag(tag) { return false; }
        }
        if let Some(ref it) = params.item_type {
            if t.get_field("item_type").as_deref() != Some(it.as_str()) {
                return false;
            }
        }
        if !t.matches_time("modified", params.modified_after.as_deref(), params.modified_before.as_deref()) {
            return false;
        }
        if !t.matches_time("created", params.created_after.as_deref(), params.created_before.as_deref()) {
            return false;
        }
        true
    }).collect();

    // 3. 按 modified 倒序排序
    results.sort_by(|a, b| {
        let ma = a.get_field("modified");
        let mb = b.get_field("modified");
        mb.cmp(&ma)
    });

    // 4. 分页（limit=0 表示不限制）
    let take_limit = if limit > 0 { limit } else { usize::MAX };
    let paged: Vec<serde_json::Value> = results.into_iter()
        .skip(offset)
        .take(take_limit)
        .map(|t| if params.include_text.unwrap_or(false) {
            t.as_value()
        } else {
            t.as_skinny_value()
        })
        .collect();

    Ok(axum::Json(paged))
}

/// GET /api/tiddlers?title=xxx — 查询参数版，无需 URL 编码
async fn get_tiddler_by_query(
    Extension(ds): Extension<DataStore>,
    extract::Query(params): extract::Query<TitleQuery>,
) -> AppResult<axum::http::Response<String>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    if let Some(t) = tiddlers.get(&params.title)? {
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

/// GET /api/inbox — 列出所有 inbox 条目
async fn list_inbox(
    Extension(ds): Extension<DataStore>,
) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    // 使用 SQL LIKE 过滤，避免全量加载
    let matched = tiddlers.get_all_by_tag("Inbox")?;
    let inbox_items: Vec<serde_json::Value> = matched.iter()
        .filter(|t| t.has_tag("Inbox"))  // 二次确认，防止 LIKE 误匹配
        .map(|t| t.as_value())
        .collect();
    Ok(axum::Json(inbox_items))
}

/// GET /api/memory/context — 记忆检索（精简输出，适合注入 Agent 上下文窗口）
async fn memory_context(
    Extension(ds): Extension<DataStore>,
    extract::Query(params): extract::Query<MemoryContextParams>,
) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let all = tiddlers.all()?;
    let limit = params.limit.unwrap_or(5);

    let now = Utc::now();
    let cutoff = params.hours.map(|h| now - chrono::Duration::hours(h as i64));

    let mut results: Vec<&Tiddler> = all.iter().filter(|t| {
        if let Some(ref q) = params.q {
            let ql = q.to_lowercase();
            let title_match = t.title.to_lowercase().contains(&ql);
            let text_match = t.get_text_field()
                .map(|txt| txt.to_lowercase().contains(&ql))
                .unwrap_or(false);
            if !title_match && !text_match { return false; }
        }
        if let Some(ref cut) = cutoff {
            if let Some(modified) = t.get_field("modified") {
                // 简单比较：TW 17 位时间戳可直接按字符串比较
                if modified.as_str() < cut.format("%Y%m%d%H%M%S%3f").to_string().as_str() {
                    return false;
                }
            }
        }
        true
    }).collect();

    results.sort_by(|a, b| {
        b.get_field("modified").cmp(&a.get_field("modified"))
    });

    // 精简输出：截断 text 到 ~500 字符；limit=0 不限制
    let output: Vec<serde_json::Value> = results.into_iter()
        .take(if limit > 0 { limit } else { usize::MAX })
        .map(|t| {
            let mut v = t.as_value();
            if let Some(obj) = v.as_object_mut() {
                if let Some(serde_json::Value::String(text)) = obj.get_mut("text") {
                    if text.len() > 500 {
                        *text = format!("{}...", &text[..500]);
                    }
                }
            }
            v
        })
        .collect();

    Ok(axum::Json(output))
}

/// GET /api/tiddlers/{title}/links — 正向链接列表
async fn tiddler_links(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
    extract::Query(params): extract::Query<SearchParams>,
) -> AppResult<axum::Json<Vec<String>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);
    let links = tiddlers.links(&title, limit, offset)?;
    Ok(axum::Json(links))
}

/// GET /api/tiddlers/{title}/backlinks — 反向链接列表
async fn tiddler_backlinks(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
    extract::Query(params): extract::Query<SearchParams>,
) -> AppResult<axum::Json<Vec<String>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);
    let links = tiddlers.backlinks(&title, limit, offset)?;
    Ok(axum::Json(links))
}

/// GET /api/tags — 所有标签及其出现次数
async fn all_tags(
    Extension(ds): Extension<DataStore>,
) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let tags = tiddlers.tags()?;
    let result: Vec<serde_json::Value> = tags.into_iter()
        .map(|(tag, count)| serde_json::json!({"tag": tag, "count": count}))
        .collect();
    Ok(axum::Json(result))
}

/// GET /api — API 自描述端点
async fn api_index() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "version": "0.5.0",
        "description": "TiddlyWiki Server — Agent-friendly API",
        "endpoints": {
            "GET /api": "This self-describing endpoint",
            "GET /api/tags": {
                "description": "List all tags with tiddler counts",
                "params": {}
            },
            "GET /api/search": {
                "description": "Search tiddlers by keyword, tag, and item_type",
                "params": {
                    "q": "Search keyword (matched against title + text)",
                    "tag": "Filter by tag",
                    "item_type": "Filter by item_type field (note/observation/backup/...)",
                    "include_text": "Include full text in results (default: false)",
                    "limit": "Page size (default: 20, 0=unlimited)",
                    "offset": "Page offset (default: 0)",
                    "modified_after": "Modified >= YYYYMMDDHHMMSSmmm",
                    "modified_before": "Modified <= YYYYMMDDHHMMSSmmm",
                    "created_after": "Created >= YYYYMMDDHHMMSSmmm",
                    "created_before": "Created <= YYYYMMDDHHMMSSmmm"
                }
            },
            "GET /api/tiddlers": {
                "description": "Get a tiddler by title using query parameter (no URL-encoding needed)",
                "params": { "title": "Tiddler title (supports Chinese characters directly)" }
            },
            "GET /api/tiddlers/tag/{tag}": {
                "description": "List tiddlers by tag",
                "params": { "include_text": "Include full text (default: false)", "limit": "Page size", "offset": "Page offset" }
            },
            "GET /api/inbox": "List all inbox items (tiddlers tagged 'Inbox')",
            "POST /api/inbox": {
                "description": "Capture a new inbox item",
                "body": {
                    "title": "string (required)",
                    "content": "string (required — alias: 'text')",
                    "text": "string (alias for 'content')",
                    "tags": ["string array"],
                    "type": "string (note/observation/backup/conclusion/...)",
                    "timestamp": "ISO 8601 string",
                    "context": "string (optional — formatted as Markdown blockquote)",
                    "metadata": "object (optional — key-value pairs flattened into tiddler fields)"
                }
            },
            "GET /api/memory/context": {
                "description": "Retrieve recent entries for agent context injection",
                "params": {
                    "q": "Search keyword (optional)",
                    "limit": "Max results (default: 5)",
                    "hours": "Time window in hours (optional)"
                }
            },
            "GET /api/sign-upload": "S3 presigned upload URL"
        }
    }))
}

/// GET /api/tiddlers/tag/{tag} — 按标签列出条目
async fn tiddlers_by_tag(
    Extension(ds): Extension<DataStore>,
    extract::Path(tag): extract::Path<String>,
    extract::Query(params): extract::Query<SearchParams>,
) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    // 使用 SQL LIKE 过滤，避免全量加载
    let matched = tiddlers.get_all_by_tag(&tag)?;

    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(50);

    let results: Vec<serde_json::Value> = matched.iter()
        .filter(|t| t.has_tag(&tag)  // 二次确认
            && t.matches_time("modified", params.modified_after.as_deref(), params.modified_before.as_deref())
            && t.matches_time("created", params.created_after.as_deref(), params.created_before.as_deref()))
        .skip(offset)
        .take(if limit > 0 { limit } else { usize::MAX })
        .map(|t| if params.include_text.unwrap_or(false) {
            t.as_value()
        } else {
            t.as_skinny_value()
        })
        .collect();

    Ok(axum::Json(results))
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
        let (status, msg) = match &self {
            AppError::Database(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            AppError::Serialization(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            AppError::Response(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
        };

        tracing::error!("{:?}", self);

        let body = serde_json::json!({
            "error": format!("{:?}", self).split('(').next().unwrap_or("unknown"),
            "message": msg,
        });

        (status, axum::Json(body)).into_response()
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
    // 合并 content 和 text 字段（接受两种 JSON 键名）
    let body = payload.content
        .or(payload.text)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Response(
            "Missing required field: 'content' (or 'text' as alias)".into()
        ))?;

    // 将 context 格式化为 Markdown 引用块并拼接到正文头部
    let final_text = match payload.context {
        Some(ctx) if !ctx.is_empty() => {
            format!("> **Context**: {}\n\n---\n\n{}", ctx, body)
        }
        _ => body,
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