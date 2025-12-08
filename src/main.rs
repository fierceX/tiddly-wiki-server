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

use axum::{
    extract,
    http::StatusCode,
    routing::{delete, get, put},
    Extension, Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;

type DataStore = Arc<Mutex<Tiddlers>>;

#[derive(Deserialize, Parser, Debug, Clone)]
#[command(about, version)]
struct AppConfig {
    /// Path of the SQLite databse to connect to.
    #[clap(
        env = "TWS_DBPATH",
        short,
        long,
        default_value = "./data/tiddlers.sqlite3"
    )]
    dbpath: PathBuf,
    /// Local IP Address to serve on (use 0.0.0.0 for all)
    #[clap(env = "TWS_BIND", short, long, default_value = "127.0.0.1")]
    bind: IpAddr,
    /// Port to bind
    #[clap(env = "TWS_PORT", short, long, default_value = "3032")]
    port: u16,
    /// Directory to serve at /files
    #[clap(env = "TWS_FILESDIR", short, long, default_value = "./files/")]
    filesdir: PathBuf,
}

// 定义一个结构体来持有预处理好的 HTML 模板部件
#[derive(Clone)]
struct WikiTemplate {
    // 包含直到 core tiddlers 结束的所有内容（不含最后一个 ']'）
    // 例如: <html>...<script...>[{"title": "$:/core"...}
    prefix: String, 
    // 包含最后一个 ']' 及其之后的所有内容
    // 例如: ]</script></body></html>
    suffix: String,
}
impl WikiTemplate {
    fn new(html_content: &str) -> Self {
        let store_marker = r#"<script class="tiddlywiki-tiddler-store" type="application/json">"#;
        
        // 1. 定位 script 标签开始
        let start_tag_idx = html_content
            .find(store_marker)
            .expect("Invalid empty.html: missing store script tag");
        
        // 2. 定位 script 标签结束 (从开始标签后面找)
        let end_tag_idx = html_content[start_tag_idx..]
            .find("</script>")
            .map(|i| start_tag_idx + i)
            .expect("Invalid empty.html: missing closing script tag");

        // 3. 定位 JSON 数组的最后一个 ']'
        // 我们在 </script> 之前倒着找最近的一个 ']'
        let split_idx = html_content[..end_tag_idx]
            .rfind(']')
            .expect("Invalid empty.html: store content is not a valid JSON array");

        // split_idx 指向 ']' 的位置
        // prefix = 0 .. split_idx (不包含 ']')
        // suffix = split_idx .. end (包含 ']')
        Self {
            prefix: html_content[..split_idx].to_string(),
            suffix: html_content[split_idx..].to_string(),
        }
    }
}

#[tokio::main]
async fn main() {
    // TODO: Instrument handlers & DB code.
    tracing_subscriber::fmt::init();

    let config = AppConfig::parse();

    let datastore = initialize_datastore(&config).expect("Error initializing datastore");
    
    let empty_html_str = include_str!("../empty.html"); 
    let template = Arc::new(WikiTemplate::new(empty_html_str));

    let addr = SocketAddr::from((config.bind, config.port));
    // This services handles the [Get File](https://tiddlywiki.com/#WebServer%20API%3A%20Get%20File)
    // API endpoint.
    let files_service = ServeDir::new(&config.filesdir);

    let app = Router::new()
        .route("/", get(render_wiki))
        .route("/status", get(status))
        .route("/recipes/default/tiddlers.json", get(all_tiddlers))
        .route(
            "/recipes/default/tiddlers/:title",
            put(put_tiddler).get(get_tiddler),
        )
        // NOTE(nknight): For some reason both the 'default' and 'efault' versions of this URL get hit.
        .route("/bags/default/tiddlers/:title", delete(delete_tiddler))
        .route("/bags/efault/tiddlers/:title", delete(delete_tiddler))
        .route_service("/files/", files_service)
        .layer(Extension(datastore))
        .layer(Extension(template));

    println!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Error binding TCP listener");
    axum::serve(listener, app).await.expect("Error serving app");
}

/// Connect to the database and run the database initialization script.
fn initialize_datastore(config: &AppConfig) -> AppResult<DataStore> {
    let init_script = include_str!("./init.sql");
    let cxn = rusqlite::Connection::open(&config.dbpath).map_err(AppError::from)?;
    cxn.execute_batch(init_script).map_err(AppError::from)?;
    let tiddlers = Tiddlers { cxn };
    Ok(Arc::new(Mutex::new(tiddlers)))
}

// -----------------------------------------------------------------------------------
// Views

///  Render the wiki as HTML, including the core modules and plugins.
///
/// Serves the [Get TiddWiki](https://tiddlywiki.com/#WebServer%20API%3A%20Get%20Wiki)
/// API endpoint.
async fn render_wiki(Extension(ds): Extension<DataStore>,Extension(template): Extension<Arc<WikiTemplate>>,) -> AppResult<axum::response::Response> {
    use axum::response::Response;

    let mut ds_lock = ds.lock().await;
    let datastore = &mut *ds_lock;

    let tiddlers: Vec<Tiddler> = datastore.all()?;

    let db_json_values: Vec<serde_json::Value> = tiddlers
        .iter()
        .map(|t| t.as_value()) 
        .collect();

    let db_json_str = serde_json::to_string(&db_json_values)
        .map_err(|e| AppError::Serialization(format!("error serializing db: {}", e)))?;

    
    // 关键步骤：处理 JSON 字符串
    // 1. 去掉首尾的 '[' 和 ']'，因为我们要把它塞进现有的数组里
    let inner_json = &db_json_str[1..db_json_str.len() - 1];

    // 2. 防止 HTML 注入攻击/破坏 (非常重要)
    // 如果 tiddler 内容里包含 "</script>"，浏览器会提前关闭标签。
    // 我们必须转义它。serde_json 默认不转义 '/'。
    // 替换 </script> 为 <\/script> 或者 \u003c/script>
    let safe_json = inner_json.replace("</script>", "<\\/script>");

    // 3. 拼接
    // 结构: prefix( ...[core_last ) + "," + db_items + suffix( ]... )
    // 注意中间加个逗号
    let mut buffer = Vec::with_capacity(template.prefix.len() + safe_json.len() + template.suffix.len() + 1);
    buffer.extend(template.prefix.as_bytes());
    buffer.push(b','); // 添加连接 core 和 db 的逗号
    buffer.extend(safe_json.as_bytes());
    buffer.extend(template.suffix.as_bytes());


    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(buffer))
        .map_err(|e| AppError::Response(format!("error building wiki: {}", e)))
}

/// Return a list of all stored tiddlers excluding the "text" field.
///
/// Corresponds to te [](https://tiddlywiki.com/#WebServer%20API%3A%20Get%20All%20Tiddlers).
async fn all_tiddlers(
    Extension(ds): Extension<DataStore>,
) -> AppResult<axum::Json<Vec<serde_json::Value>>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    let all: Vec<serde_json::Value> = tiddlers
        .all()?
        .iter()
        .map(|t| t.as_skinny_value())
        .collect();
    Ok(axum::Json(all))
}

/// Retrieve a single tiddler by title.
///
/// Serves the [Get Tiddler](https://tiddlywiki.com/#WebServer%20API%3A%20Get%20Tiddler)
/// API endpoint.
async fn get_tiddler(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
) -> AppResult<axum::http::Response<String>> {
    use serde_json::ser::to_string_pretty;

    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;

    if let Some(t) = tiddlers.get(&title)? {
        let body = to_string_pretty(&t.as_value())
            .map_err(|e| AppError::Serialization(format!("error serializing tiddler: {}", e)))?;
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(body)
            .map_err(|e| AppError::Response(format!("error building response: {}", e)))
    } else {
        let body = String::new();
        axum::response::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(body)
            .map_err(|e| AppError::Response(format!("error building 404 response: {}", e)))
    }
}

/// Delete a tiddler by title.
///
/// Serves the [Delete Tiddler](https://tiddlywiki.com/#WebServer%20API%3A%20Delete%20Tiddler).
/// API endpoint.
async fn delete_tiddler(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
) -> AppResult<axum::response::Response<String>> {
    let mut lock = ds.lock().await;
    let tiddlers = &mut *lock;
    tiddlers.pop(&title)?;

    let mut resp = axum::response::Response::default();
    *resp.status_mut() = StatusCode::NO_CONTENT;
    Ok(resp)
}

/// Create or update a single Tiddler.
///
/// Serves the [Put Tiddler](https://tiddlywiki.com/#WebServer%20API%3A%20Put%20Tiddler)
/// API endpoint.
async fn put_tiddler(
    Extension(ds): Extension<DataStore>,
    extract::Path(title): extract::Path<String>,
    extract::Json(v): extract::Json<serde_json::Value>,
) -> AppResult<axum::http::Response<String>> {
    use axum::http::response::Response;
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
// Models and serialization/parsing

pub(crate) struct Tiddlers {
    cxn: rusqlite::Connection,
}

impl Tiddlers {
    pub(crate) fn all(&self) -> AppResult<Vec<Tiddler>> {
        tracing::debug!("Retrieving all tiddlers");
        const GET: &str = r#"
            SELECT title, revision, meta FROM tiddlers
        "#;
        let mut stmt = self.cxn.prepare_cached(GET).map_err(AppError::from)?;
        let raw_tiddlers = stmt
            .query_map([], |r| r.get::<usize, serde_json::Value>(2))
            .map_err(AppError::from)?;
        let mut tiddlers = Vec::new();
        for qt in raw_tiddlers {
            let raw = qt.map_err(AppError::from)?;
            let tiddler = Tiddler::from_value(raw)?;
            tiddlers.push(tiddler);
        }
        Ok(tiddlers)
    }

    pub(crate) fn get(&self, title: &str) -> AppResult<Option<Tiddler>> {
        use rusqlite::OptionalExtension;

        tracing::debug!("getting tiddler: {}", title);

        const GET: &str = r#"
            SELECT title, revision, meta FROM tiddlers
            WHERE title = ?
        "#;
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
        let mut stmt = self
            .cxn
            .prepare_cached(PUT)
            .map_err(|e| AppError::Database(format!("Error preparing statement: {}", e)))?;
        stmt.execute(rusqlite::named_params! {
            ":title": tiddler.title,
            ":revision": tiddler.revision,
            ":meta": tiddler.meta,
        })?;
        tracing::debug!("done");
        Ok(())
    }

    pub(crate) fn pop(&mut self, title: &str) -> AppResult<Option<Tiddler>> {
        tracing::debug!("popping tiddler: {}", title);
        let result = self.get(title)?;
        const DELETE: &str = "DELETE FROM tiddlers WHERE title = :title";
        let mut stmt = self
            .cxn
            .prepare(DELETE)
            .map_err(|e| AppError::Database(format!("Error preparing {}: {}", DELETE, e)))?;
        stmt.execute(rusqlite::named_params! { ":title": title })
            .map_err(|e| AppError::Database(format!("Error removing tiddler: {}", e)))?;
        Ok(result)
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Tiddler {
    title: String,
    revision: u64,
    meta: serde_json::Value,
}

impl Tiddler {
    pub(crate) fn as_value(&self) -> Value {

        let mut meta = self.meta.clone();
        if let Value::Object(ref mut map) = meta {
            // 1. 展平 fields
            if let Some(Value::Object(fields)) = map.remove("fields") {
                for (k, v) in fields {
                    map.entry(k).or_insert(v);
                }
            }

        if let Some(tags_val) = map.get("tags") {
                match tags_val {
                    // 如果它是 Array，我们需要把它变成 "tag1 [[tag 2]]" 这种格式
                    Value::Array(arr) => {
                        let tag_str = arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| {
                                // 如果标签含有空格，需要用双中括号包起来
                                if s.contains(' ') {
                                    format!("[[{}]]", s)
                                } else {
                                    s.to_string()
                                }
                            })
                            .collect::<Vec<String>>()
                            .join(" ");
                        
                        // 覆盖原有的 List，存为 String
                        map.insert("tags".to_string(), Value::String(tag_str));
                    },
                    // 如果它已经是 String，保持不变（或者是错的但我们暂且信任数据库）
                    Value::String(_) => {}, 
                    // 其他情况（如 null），删除或忽略
                    _ => { map.remove("tags"); }
                }
            }

            // 2. 强制覆盖关键字段
            map.insert("title".to_string(), Value::String(self.title.clone()));
            map.insert("revision".to_string(), Value::String(self.revision.to_string()));
            
            // 3. 【建议】添加 bag 字段，这对 syncer 很重要
            // 默认情况下 TiddlyWiki 认为条目属于 "default" bag
            map.entry("bag".to_string()).or_insert(Value::String("default".to_string()));
        }

        meta
    }

    /// Serialize the Tiddler as JSON, removing the `text` field (used to
    /// efficiently get a list of tiddlers to the web frontend).
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
            _ => {
                return Err(AppError::Serialization(
                    "from_value expects a JSON Object".to_string(),
                ))
            }
        };
        let title = match obj.get("title") {
            Some(Value::String(s)) => s,
            _ => {
                return Err(AppError::Serialization(
                    "tiddler['title'] should be a string".to_string(),
                ))
            }
        };
        let revision = match obj.get("revision") {
            None => 0,
            Some(Value::Number(n)) => n.as_u64().ok_or_else(|| {
                AppError::Serialization(format!("revision should be a u64 (not {})", n))
            })?,
            Some(Value::String(s)) => s.parse::<u64>().map_err(|_| {
                AppError::Serialization(format!("couldn't parse a revision number from '{}'", s))
            })?,
            _ => {
                return Err(AppError::Serialization(
                    "tiddler['revision'] should be a number".to_string(),
                ))
            }
        };
        let tiddler = Tiddler {
            title: title.clone(),
            revision,
            meta: value,
        };
        Ok(tiddler)
    }
}

// -----------------------------------------------------------------------------------
// Static Status

#[derive(Serialize)]
struct Status {
    username: &'static str,
    anonymous: bool,
    read_only: bool,
    space: Space,
    tiddlywiki_version: &'static str,
}

#[derive(Serialize)]
struct Space {
    recipe: &'static str,
}

// TODO(nknight): Make this configurable (or support the features it describes).
const STATUS: Status = Status {
    username: "fiercex",
    anonymous: false,
    read_only: false,
    space: Space { recipe: "default" },
    tiddlywiki_version: "5.3.8",
};

/// Return the server status as JSON.
///
/// Serves the [Get Server Stats](https://tiddlywiki.com/#WebServer%20API%3A%20Get%20Server%20Status)
/// API endpoint.
async fn status() -> axum::Json<Status> {
    axum::Json(STATUS)
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
        let msg = err.to_string();
        AppError::Database(msg)
    }
}
