//! Wiki CLI — Rust 原生实现，无 Python 依赖
//!
//! 用法: wiki <子命令> [参数]
//!
//! 环境变量:
//!   WIKI_SERVER_URL  (默认 http://localhost:3032)
//!   WIKI_USERNAME
//!   WIKI_PASSWORD
//!   WIKI_BACKUP_CONFIG (default config.toml) — 本地备份时指定配置文件路径

use clap::{Parser, Subcommand};
use chrono::{Utc, Local};
use reqwest::blocking::Client;
use rusqlite::backup::Backup;
use rusqlite::Connection;
use serde::{Deserialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

// ─── CLI 参数结构 ──────────────────────────────────────────

#[derive(Parser)]
#[command(name = "wiki", version, about = "TiddlyWiki CLI tool (Rust)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 搜索条目
    Search {
        query: String,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long = "type")]
        item_type: Option<String>,
        #[arg(long)]
        full: bool,
        #[arg(long, default_value = "20")]
        limit: usize,
        #[arg(long, default_value = "0")]
        offset: usize,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        modified_after: Option<String>,
        #[arg(long)]
        modified_before: Option<String>,
        #[arg(long)]
        created_after: Option<String>,
        #[arg(long)]
        created_before: Option<String>,
        #[arg(long)]
        plain: bool,
    },
    /// 获取单个条目
    Get {
        title: String,
        #[arg(long)]
        text_only: bool,
    },
    /// 创建/更新条目
    Put {
        title: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        tags: Option<String>,
        #[arg(long = "type", default_value = "note")]
        item_type: String,
    },
    /// 快速采集到 Inbox
    Inbox {
        title: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        tags: Option<String>,
        #[arg(long = "type", default_value = "note")]
        item_type: String,
        #[arg(long)]
        context: Option<String>,
    },
    /// 列出条目
    List {
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long)]
        inbox: bool,
        #[arg(long)]
        plain: bool,
    },
    /// 删除条目
    Delete {
        title: String,
        #[arg(long)]
        force: bool,
    },
    /// 正向链接
    Links {
        title: String,
        #[arg(long)]
        plain: bool,
    },
    /// 反向链接
    Backlinks {
        title: String,
        #[arg(long)]
        plain: bool,
    },
    /// 批量查询链接
    BatchLinks {
        titles: Vec<String>,
        #[arg(long)]
        plain: bool,
    },
    /// 所有标签及计数
    Tags {
        #[arg(long)]
        plain: bool,
    },
    /// 列出最近修改
    Changes {
        #[arg(long, default_value = "24h")]
        since: String,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long)]
        plain: bool,
    },
    /// BFS 链接图谱
    Graph {
        start: String,
        #[arg(long, default_value = "2")]
        depth: usize,
        #[arg(long)]
        plain: bool,
    },
    /// 备份操作（直接访问数据库，不依赖 HTTP API）
    Backup {
        #[command(subcommand)]
        action: BackupAction,
        /// 配置文件路径（用于读取 db_path）
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// 导出条目（直接访问数据库，不依赖 HTTP API）
    Export {
        #[command(subcommand)]
        action: ExportAction,
        /// 配置文件路径（用于读取 db_path）
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// 导入条目（从 HTML 或 .tid 文件夹导入到数据库）
    Import {
        #[command(subcommand)]
        action: ImportAction,
        /// 配置文件路径（用于读取 db_path）
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
}

#[derive(Subcommand)]
enum ExportAction {
    /// 导出为独立 TiddlyWiki HTML 文件（可浏览器直接打开）
    Html {
        /// 输出 HTML 文件路径（默认: wiki_export_<日期>.html）
        #[arg(short, long)]
        output: Option<String>,
    },
    /// 导出为 .tid 文件夹（Node.js TiddlyWiki 服务器兼容）
    Folder {
        /// 输出目录路径（默认: wiki_export_<日期>/）
        #[arg(short, long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum ImportAction {
    /// 从 TiddlyWiki HTML 文件导入
    Html {
        /// 输入的 HTML 文件路径
        #[arg(short, long)]
        input: String,
    },
    /// 从 .tid 文件夹导入（Node.js TiddlyWiki 服务器兼容）
    Folder {
        /// 输入的文件夹路径
        #[arg(short, long)]
        input: String,
    },
}

#[derive(Subcommand)]
enum BackupAction {
    /// 导出 SQLite 数据库文件（一致性快照，通过 SQLite backup API）
    Db {
        /// 输出文件路径（默认: tiddlers_backup_<日期>.db）
        #[arg(short, long)]
        output: Option<String>,
    },
    /// 导出所有条目为标准 TiddlyWiki JSON 格式
    Tiddlers {
        /// 输出文件路径（默认: tiddlers_export_<日期>.json）
        #[arg(short, long)]
        output: Option<String>,
    },
}

// ─── Wiki API 客户端 ───────────────────────────────────────

struct WikiClient {
    base_url: String,
    client: Client,
}

impl WikiClient {
    fn from_env() -> Self {
        let base_url = std::env::var("WIKI_SERVER_URL")
            .unwrap_or_else(|_| "http://localhost:3032".to_string());
        let username = std::env::var("WIKI_USERNAME").unwrap_or_default();
        let _password = std::env::var("WIKI_PASSWORD").unwrap_or_default();

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("wiki-cli-rust/0.1")
            .build()
            .expect("failed to build HTTP client");

        let wc = WikiClient { base_url, client };

        if !username.is_empty() {
            wc // auth is set per-request
        } else {
            wc
        }
    }

    fn auth(&self) -> Option<String> {
        let u = std::env::var("WIKI_USERNAME").ok()?;
        let p = std::env::var("WIKI_PASSWORD").ok()?;
        if u.is_empty() || p.is_empty() {
            return None;
        }
        use base64::Engine;
        let creds = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", u, p));
        Some(format!("Basic {}", creds))
    }

    fn get_json(&self, path: &str, params: &[(&str, String)]) -> Result<Value, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        for (k, v) in params {
            req = req.query(&[(k, v)]);
        }
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        let resp = req.send().map_err(|e| format!("HTTP error: {}", e))?;
        let status = resp.status();
        let body = resp.text().map_err(|e| format!("read error: {}", e))?;
        if !status.is_success() {
            return Err(format!("HTTP {} — {}", status, body));
        }
        serde_json::from_str(&body).map_err(|e| format!("JSON error: {}", e))
    }

    fn put_json(&self, path: &str, data: &Value) -> Result<u16, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.put(&url).json(data);
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        let resp = req.send().map_err(|e| format!("HTTP error: {}", e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(format!("HTTP {} — {}", status, body));
        }
        Ok(status.as_u16())
    }

    fn post_json(&self, path: &str, data: &Value) -> Result<Value, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.post(&url).json(data);
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        let resp = req.send().map_err(|e| format!("HTTP error: {}", e))?;
        let status = resp.status();
        let body = resp.text().map_err(|e| format!("read error: {}", e))?;
        if !status.is_success() {
            return Err(format!("HTTP {} — {}", status, body));
        }
        serde_json::from_str(&body).map_err(|e| format!("JSON error: {}", e))
    }

    fn delete_req(&self, path: &str) -> Result<u16, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.delete(&url);
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        let resp = req.send().map_err(|e| format!("HTTP error: {}", e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(format!("HTTP {} — {}", status, body));
        }
        Ok(status.as_u16())
    }

    // ─── API 方法 ──────────────────────────────────────

    fn search(&self, query: &str, full: bool, limit: usize, offset: usize,
              tag: Option<&str>, item_type: Option<&str>, mode: Option<&str>,
              modified_after: Option<&str>, modified_before: Option<&str>,
              created_after: Option<&str>, created_before: Option<&str>) -> Result<Vec<Value>, String> {
        let mut params: Vec<(&str, String)> = vec![
            ("q", query.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if full { params.push(("include_text", "true".into())); }
        if let Some(v) = tag { params.push(("tag", v.into())); }
        if let Some(v) = item_type { params.push(("item_type", v.into())); }
        if let Some(v) = mode { params.push(("mode", v.into())); }
        if let Some(v) = modified_after { params.push(("modified_after", v.into())); }
        if let Some(v) = modified_before { params.push(("modified_before", v.into())); }
        if let Some(v) = created_after { params.push(("created_after", v.into())); }
        if let Some(v) = created_before { params.push(("created_before", v.into())); }

        let val = self.get_json("/api/search", &params)?;
        Ok(val.as_array().cloned().unwrap_or_default())
    }

    fn get(&self, title: &str) -> Result<Value, String> {
        let title_enc = urlencoding::encode(title);
        self.get_json(&format!("/api/tiddlers?title={}", title_enc), &[])
    }

    fn put(&self, title: &str, content: &str, tags: Option<&str>, _item_type: &str) -> Result<u16, String> {
        // 先尝试获取现有条目获取 revision
        let (revision, created, creator) = match self.get(title) {
            Ok(v) => {
                let rev = v.get("revision").and_then(|r| r.as_str()).unwrap_or("0").to_string();
                let cr = v.get("created").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let ctor = v.get("creator").and_then(|c| c.as_str()).unwrap_or("").to_string();
                (rev, cr, ctor)
            }
            Err(_) => ("0".into(), "".into(), "".into()),
        };

        let username = std::env::var("WIKI_USERNAME").unwrap_or_default();

        // 不发送 created/modified，由服务器填充服务器时间
        let mut payload = json!({
            "title": title,
            "text": content,
            "type": "text/markdown",
            "tags": tags.unwrap_or(""),
            "revision": revision,
            "modifier": username,
        });

        // 新建时补充创建元数据
        let is_new = revision == "0";
        if is_new {
            let map = payload.as_object_mut().unwrap();
            map.insert("creator".into(), json!(username));
        } else {
            if !created.is_empty() {
                let map = payload.as_object_mut().unwrap();
                map.insert("created".into(), json!(created));
            }
            if !creator.is_empty() {
                let map = payload.as_object_mut().unwrap();
                map.insert("creator".into(), json!(creator));
            }
        }
        let title_enc = urlencoding::encode(title);
        self.put_json(&format!("/recipes/default/tiddlers/{}", title_enc), &payload)
    }

    fn list(&self, tag: Option<&str>, limit: usize) -> Result<Vec<Value>, String> {
        if let Some(t) = tag {
            let t_enc = urlencoding::encode(t);
            self.get_json(&format!("/api/tiddlers/tag/{}", t_enc),
                          &[("limit", limit.to_string())])
                .map(|v| v.as_array().cloned().unwrap_or_default())
        } else {
            self.get_json("/recipes/default/tiddlers.json", &[])
                .map(|v| v.as_array().cloned().unwrap_or_default())
        }
    }

    fn list_inbox(&self) -> Result<Vec<Value>, String> {
        self.get_json("/api/inbox", &[])
            .map(|v| v.as_array().cloned().unwrap_or_default())
    }

    fn delete(&self, title: &str) -> Result<u16, String> {
        let title_enc = urlencoding::encode(title);
        self.delete_req(&format!("/bags/default/tiddlers/{}", title_enc))
    }

    fn links(&self, title: &str) -> Result<Vec<String>, String> {
        let title_enc = urlencoding::encode(title);
        let val = self.get_json(&format!("/api/tiddlers/{}/links", title_enc), &[])?;
        Ok(val.as_array().map(|a| {
            a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
        }).unwrap_or_default())
    }

    fn backlinks(&self, title: &str) -> Result<Vec<String>, String> {
        let title_enc = urlencoding::encode(title);
        let val = self.get_json(&format!("/api/tiddlers/{}/backlinks", title_enc), &[])?;
        Ok(val.as_array().map(|a| {
            a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
        }).unwrap_or_default())
    }

    fn tags(&self) -> Result<Vec<Value>, String> {
        self.get_json("/api/tags", &[])
            .map(|v| v.as_array().cloned().unwrap_or_default())
    }
}

// ─── 命令处理函数 ──────────────────────────────────────────

fn cmd_search(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Search { query, tag, item_type, full, limit, offset, mode,
        modified_after, modified_before, created_after, created_before, plain } = args else {
        unreachable!()
    };
    let results = client.search(query, *full, *limit, *offset,
        tag.as_deref(), item_type.as_deref(), mode.as_deref(),
        modified_after.as_deref(), modified_before.as_deref(),
        created_after.as_deref(), created_before.as_deref())?;
    if *plain {
        for r in &results {
            if let Some(title) = r.get("title").and_then(|v| v.as_str()) {
                println!("{}", title);
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    }
    Ok(())
}

fn cmd_get(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Get { title, text_only } = args else { unreachable!() };
    let result = client.get(title)?;
    if *text_only {
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            println!("{}", text);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    }
    Ok(())
}

fn cmd_put(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Put { title, content, file, tags, item_type } = args else { unreachable!() };
    let body = if let Some(f) = file {
        fs::read_to_string(f).map_err(|e| format!("read file error: {}", e))?
    } else if let Some(c) = content {
        c.clone()
    } else {
        return Err("需要 --content 或 --file 提供正文".into());
    };
    client.put(title, &body, tags.as_deref(), item_type)?;
    println!("wiki: '{}' 写入成功", title);
    Ok(())
}

fn cmd_inbox(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Inbox { title, content, file, tags, item_type, context } = args else { unreachable!() };
    let body = if let Some(f) = file {
        fs::read_to_string(f).map_err(|e| format!("read file error: {}", e))?
    } else if let Some(c) = content {
        c.clone()
    } else {
        return Err("需要 --content 或 --file 提供正文".into());
    };
    let payload = json!({
        "title": title,
        "content": body,
        "tags": tags.as_deref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect::<Vec<_>>()).unwrap_or_default(),
        "type": item_type,
        "context": context.as_deref().unwrap_or(""),
    });
    let result = client.post_json("/api/inbox", &payload)?;
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

fn cmd_list(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::List { tag, limit, inbox, plain } = args else { unreachable!() };
    let results = if *inbox {
        client.list_inbox()?
    } else if let Some(t) = tag {
        client.list(Some(t), *limit)?
    } else {
        client.list(None, *limit)?
    };
    if *plain {
        for r in &results {
            let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let tags = r.get("tags").and_then(|v| v.as_str()).unwrap_or("");
            println!("{}  [{}]", title, tags);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    }
    Ok(())
}

fn cmd_delete(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Delete { title, force } = args else { unreachable!() };
    if !*force {
        return Err(format!("确认删除 '{}'? 使用 --force 跳过确认", title));
    }
    client.delete(title)?;
    println!("wiki: '{}' 已删除", title);
    Ok(())
}

fn cmd_links(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Links { title, plain } = args else { unreachable!() };
    let results = client.links(title)?;
    if *plain {
        for t in &results {
            println!("{}", t);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    }
    Ok(())
}

fn cmd_backlinks(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Backlinks { title, plain } = args else { unreachable!() };
    let results = client.backlinks(title)?;
    if *plain {
        for t in &results {
            println!("{}", t);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    }
    Ok(())
}

fn cmd_batch_links(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::BatchLinks { titles, plain } = args else { unreachable!() };
    let mut result = HashMap::new();
    for t in titles {
        let links = client.links(t).unwrap_or_default();
        result.insert(t.clone(), links);
    }
    if *plain {
        for (source, targets) in &result {
            if !targets.is_empty() {
                println!("{}:", source);
                for tg in targets {
                    println!("  → {}", tg);
                }
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    }
    Ok(())
}

fn cmd_tags(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Tags { plain } = args else { unreachable!() };
    let results = client.tags()?;
    if *plain {
        for r in &results {
            let tag = r.get("tag").and_then(|v| v.as_str()).unwrap_or("");
            let count = r.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{:30}  {:>4}", tag, count);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    }
    Ok(())
}

fn cmd_changes(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Changes { since, tag, limit, plain } = args else { unreachable!() };

    // 解析 --since
    let modified_after = if since.ends_with('h') {
        let hours: i64 = since[..since.len()-1].parse().unwrap_or(24);
        let ts = Utc::now() - chrono::Duration::hours(hours);
        Some(ts.format("%Y%m%d%H%M%S").to_string() + "000")
    } else if since.ends_with('d') {
        let days: i64 = since[..since.len()-1].parse().unwrap_or(1);
        let ts = Utc::now() - chrono::Duration::days(days);
        Some(ts.format("%Y%m%d%H%M%S").to_string() + "000")
    } else {
        Some(since.clone())
    };

    let results = client.search("", false, *limit, 0,
        tag.as_deref(), None, None,
        modified_after.as_deref(), None, None, None)?;

    if *plain {
        for r in &results {
            let mod_ts = r.get("modified").and_then(|v| v.as_str()).unwrap_or("");
            let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
            println!("{}  {}", &mod_ts[..8.min(mod_ts.len())], title);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    }
    Ok(())
}

fn cmd_graph(client: &WikiClient, args: &Commands) -> Result<(), String> {
    let Commands::Graph { start, depth, plain } = args else { unreachable!() };

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();

    queue.push_back((start.clone(), 0));

    while let Some((title, d)) = queue.pop_front() {
        if visited.contains(&title) || d > *depth {
            continue;
        }
        visited.insert(title.clone());
        match client.links(&title) {
            Ok(targets) => {
                graph.insert(title.clone(), targets.clone());
                if d < *depth {
                    for t in targets {
                        if !visited.contains(&t) {
                            queue.push_back((t, d + 1));
                        }
                    }
                }
            }
            Err(_) => {
                graph.insert(title.clone(), vec![]);
            }
        }
    }

    if *plain {
        for (source, targets) in &graph {
            if targets.is_empty() {
                println!("{}  →  (无链接)", source);
            } else {
                println!("{}  →  {}", source, targets.join(", "));
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&graph).unwrap());
    }
    Ok(())
}

// ─── 备份工具 ────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct CliServerConfig {
    db_path: PathBuf,
}

#[derive(Deserialize, Debug)]
struct CliAppConfig {
    server: CliServerConfig,
}

fn load_cli_config(config_path: &std::path::Path) -> Result<CliAppConfig, String> {
    let content = fs::read_to_string(config_path)
        .map_err(|e| format!("无法读取配置文件 '{}': {}", config_path.display(), e))?;
    toml::from_str(&content)
        .map_err(|e| format!("解析配置文件失败: {}", e))
}

fn default_backup_db_path() -> String {
    let now = Local::now().format("%Y%m%d_%H%M%S");
    format!("tiddlers_backup_{}.db", now)
}

fn default_backup_tiddlers_path() -> String {
    let now = Local::now().format("%Y%m%d_%H%M%S");
    format!("tiddlers_export_{}.json", now)
}

fn cmd_backup(config_path: &std::path::Path, action: &BackupAction) -> Result<(), String> {
    let cfg = load_cli_config(config_path)?;
    let db_path = cfg.server.db_path;

    match action {
        BackupAction::Db { output } => {
            let out_path = output.clone().unwrap_or_else(default_backup_db_path);
            // 用嵌套块确保 backup borrow 在 close 前结束
            {
                let src = Connection::open(&db_path)
                    .map_err(|e| format!("无法打开源数据库 {:?}: {}", db_path, e))?;
                let mut dst = Connection::open(&out_path)
                    .map_err(|e| format!("无法创建备份文件 '{}': {}", out_path, e))?;
                let backup = Backup::new(&src, &mut dst)
                    .map_err(|e| format!("备份初始化失败: {}", e))?;
                backup.run_to_completion(100, std::time::Duration::from_millis(250), None)
                    .map_err(|e| format!("备份执行失败: {}", e))?;
                // block end → backup, dst, src 按逆序 drop
            }

            let size = std::fs::metadata(&out_path)
                .map(|m| m.len())
                .unwrap_or(0);
            println!("✅ 数据库备份完成: {}", out_path);
            println!("   大小: {} bytes", size);
        }
        BackupAction::Tiddlers { output } => {
            let out_path = output.clone().unwrap_or_else(default_backup_tiddlers_path);
            let conn = Connection::open(&db_path)
                .map_err(|e| format!("无法打开数据库 {:?}: {}", db_path, e))?;

            let mut stmt = conn.prepare("SELECT meta FROM tiddlers")
                .map_err(|e| format!("查询失败: {}", e))?;
            let rows: Vec<serde_json::Value> = stmt.query_map([], |r| {
                r.get::<usize, serde_json::Value>(0)
            }).map_err(|e| format!("读取数据失败: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

            let json_out = serde_json::to_string_pretty(&rows)
                .map_err(|e| format!("序列化失败: {}", e))?;
            fs::write(&out_path, &json_out)
                .map_err(|e| format!("写入文件失败 '{}': {}", out_path, e))?;

            println!("✅ 条目导出完成: {}", out_path);
            println!("   条目数: {}", rows.len());
        }
    }
    Ok(())
}

// ─── 导出工具 ────────────────────────────────────────────────

fn cmd_export_html(config_path: &std::path::Path, action: &ExportAction) -> Result<(), String> {
    let cfg = load_cli_config(config_path)?;
    let db_path = cfg.server.db_path;

    let ExportAction::Html { output } = action else { unreachable!() };
    let out_path = output.clone().unwrap_or_else(|| {
        let now = Local::now().format("%Y%m%d_%H%M%S");
        format!("wiki_export_{}.html", now)
    });

    // 1. 从数据库中读取所有条目并规范化（与服务器 render_wiki 的 as_value 一致）
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("无法打开数据库 {:?}: {}", db_path, e))?;
    let mut stmt = conn.prepare("SELECT title, revision, meta FROM tiddlers")
        .map_err(|e| format!("查询失败: {}", e))?;
    let tiddlers: Vec<(String, u64, serde_json::Value)> = stmt.query_map([], |r| {
        Ok((r.get::<usize, String>(0)?, r.get::<usize, u64>(1)?, r.get::<usize, serde_json::Value>(2)?))
    }).map_err(|e| format!("读取数据失败: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    // 对每个条目应用 as_value 转换（确保 title/revision/bag 字段正确）
    let mut normalized: Vec<serde_json::Value> = Vec::with_capacity(tiddlers.len());
    for (title, revision, meta) in &tiddlers {
        if let Some(obj) = meta.as_object() {
            let mut map = obj.clone();
            // 展平 fields 子对象
            if let Some(serde_json::Value::Object(fields)) = map.remove("fields") {
                for (k, v) in fields {
                    map.entry(k).or_insert(v);
                }
            }
            // tags 数组转字符串
            if let Some(tags_val) = map.get("tags") {
                match tags_val {
                    serde_json::Value::Array(arr) => {
                        let tag_str: Vec<String> = arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| if s.contains(' ') { format!("[[{}]]", s) } else { s.to_string() })
                            .collect();
                        map.insert("tags".to_string(), serde_json::Value::String(tag_str.join(" ")));
                    },
                    _ => {}
                }
            }
            // 确保 title/revision/bag 存在
            map.insert("title".to_string(), serde_json::Value::String(title.clone()));
            map.insert("revision".to_string(), serde_json::Value::String(revision.to_string()));
            map.entry("bag".to_string()).or_insert(serde_json::Value::String("default".to_string()));
            normalized.push(serde_json::Value::Object(map));
        } else {
            // 非 Object 的 meta 原始输出
            normalized.push(meta.clone());
        }
    }
    // 2. 解析模板中的 store JSON，与数据库条目合并（数据库版本覆盖模板版本）
    let html_content = include_str!("../empty.html");
    let store_marker = r#"<script class="tiddlywiki-tiddler-store" type="application/json">"#;
    let start_tag_idx = html_content.find(store_marker)
        .ok_or_else(|| "Invalid empty.html: missing store script tag".to_string())?;
    let tag_open_end = start_tag_idx + store_marker.len();
    let close_tag_idx = html_content[tag_open_end..]
        .find("</script>")
        .map(|i| tag_open_end + i)
        .ok_or_else(|| "Invalid empty.html: missing closing script tag".to_string())?;

    let template_store: Vec<serde_json::Value> = serde_json::from_str(
        html_content[tag_open_end..close_tag_idx].trim()
    ).map_err(|e| format!("解析模板 store JSON 失败: {}", e))?;

    // 用数据库条目覆盖同名的模板条目（保持 $:/core 等核心条目）
    let mut merged: Vec<serde_json::Value> = template_store;
    let mut db_by_title: std::collections::HashMap<&str, &serde_json::Value> = std::collections::HashMap::new();
    for t in &normalized {
        if let Some(title) = t.get("title").and_then(|v| v.as_str()) {
            db_by_title.insert(title, t);
        }
    }
    for t in merged.iter_mut() {
        if let Some(title) = t.get("title").and_then(|v| v.as_str()) {
            if let Some(db_val) = db_by_title.get(title) {
                *t = (*db_val).clone();
            }
        }
    }
    // 添加仅存在于数据库的条目（克隆标题集合避免借用冲突）
    let merged_titles: std::collections::HashSet<String> = merged.iter()
        .filter_map(|t| t.get("title").and_then(|v| v.as_str()).map(String::from))
        .collect();
    for t in &normalized {
        if let Some(title) = t.get("title").and_then(|v| v.as_str()) {
            if !merged_titles.contains(title) {
                merged.push(t.clone());
            }
        }
    }

    // 3. 构建完整 HTML
    let prefix = &html_content[..tag_open_end];
    let suffix = &html_content[close_tag_idx..];

    let merged_json = serde_json::to_string(&merged)
        .map_err(|e| format!("序列化失败: {}", e))?;
    let safe_json = merged_json.replace("</script>", "<\\/script>");

    let mut buffer = String::with_capacity(prefix.len() + safe_json.len() + suffix.len() + 2);
    buffer.push_str(prefix);
    buffer.push('\n');
    buffer.push_str(&safe_json);
    buffer.push('\n');
    buffer.push_str(suffix);

    fs::write(&out_path, &buffer)
        .map_err(|e| format!("写入文件失败 '{}': {}", out_path, e))?;

    println!("✅ HTML 导出完成: {}", out_path);
    println!("   条目数: {}", normalized.len());
    Ok(())
}

fn cmd_export_folder(config_path: &std::path::Path, action: &ExportAction) -> Result<(), String> {
    let cfg = load_cli_config(config_path)?;
    let db_path = cfg.server.db_path;

    let ExportAction::Folder { output } = action else { unreachable!() };
    let out_dir = output.clone().unwrap_or_else(|| {
        let now = Local::now().format("%Y%m%d_%H%M%S");
        format!("wiki_export_{}", now)
    });

    let conn = Connection::open(&db_path)
        .map_err(|e| format!("无法打开数据库 {:?}: {}", db_path, e))?;
    let mut stmt = conn.prepare("SELECT title, meta FROM tiddlers")
        .map_err(|e| format!("查询失败: {}", e))?;
    let tiddlers: Vec<(String, serde_json::Value)> = stmt.query_map([], |r| {
        Ok((r.get::<usize, String>(0)?, r.get::<usize, serde_json::Value>(1)?))
    }).map_err(|e| format!("读取数据失败: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("创建目录失败 '{}': {}", out_dir, e))?;

    let mut count = 0usize;
    for (title, meta) in &tiddlers {
        let safe_title = sanitize_filename(title);
        let tid_path = std::path::Path::new(&out_dir).join(format!("{}.tid", safe_title));

        // 构建 .tid 文件内容：header 字段 + 空行 + body
        let mut content = String::new();
        content.push_str(&format!("title: {}\n", title));

        // 提取常用元数据字段
        let meta_obj = match meta {
            serde_json::Value::Object(m) => m,
            _ => continue,
        };
        for field in &["tags", "created", "modified", "creator", "modifier", "type"] {
            if let Some(serde_json::Value::String(val)) = meta_obj.get(*field) {
                content.push_str(&format!("{}: {}\n", field, val));
            }
        }

        // 字段转写：text 是正文，用空行分隔
        if let Some(serde_json::Value::String(text)) = meta_obj.get("text") {
            content.push('\n');
            content.push_str(text);
        } else {
            content.push('\n');
        }

        fs::write(&tid_path, &content)
            .map_err(|e| format!("写入 '{}' 失败: {}", safe_title, e))?;
        count += 1;
    }

    println!("✅ 文件夹导出完成: {}", out_dir);
    println!("   条目数: {}", count);
    Ok(())
}

fn sanitize_filename(name: &str) -> String {
    let mut safe = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' => safe.push('_'),
            _ => safe.push(ch),
        }
    }
    if safe.is_empty() { safe = "_".to_string(); }
    safe
}

// ─── 导入工具 ────────────────────────────────────────────────

fn cmd_import_html(config_path: &std::path::Path, action: &ImportAction) -> Result<(), String> {
    let cfg = load_cli_config(config_path)?;
    let db_path = cfg.server.db_path;

    let ImportAction::Html { input } = action else { unreachable!() };

    let html = fs::read_to_string(input)
        .map_err(|e| format!("无法读取 HTML 文件 '{}': {}", input, e))?;

    // 1. 提取 JSON store
    let store_marker = r#"<script class="tiddlywiki-tiddler-store" type="application/json">"#;
    let start_idx = html.find(store_marker)
        .ok_or_else(|| "HTML 中未找到 tiddler store script tag".to_string())?;
    let json_start = start_idx + store_marker.len();
    let json_end = html[json_start..].find("</script>")
        .map(|i| json_start + i)
        .ok_or_else(|| "未找到 closing script tag".to_string())?;
    let json_str = &html[json_start..json_end];

    let tiddlers: Vec<serde_json::Value> = serde_json::from_str(json_str)
        .map_err(|e| format!("解析 tiddler JSON 失败: {}", e))?;

    // 2. 逐个导入
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("无法打开数据库 {:?}: {}", db_path, e))?;
    let mut insert_count = 0usize;
    let mut skip_count = 0usize;

    for tid in &tiddlers {
        let title = match tid.get("title").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => { skip_count += 1; continue; }
        };

        // 构建完整的 tiddler JSON（包含 revision）
        let mut tiddler_obj = tid.clone();
        if let Some(obj) = tiddler_obj.as_object_mut() {
            if !obj.contains_key("revision") {
                obj.insert("revision".to_string(), serde_json::Value::Number(serde_json::Number::from(0u64)));
            }
        }
        let revision: u64 = tiddler_obj.get("revision").map_or(0, |v| match v {
            serde_json::Value::String(s) => s.parse::<u64>().unwrap_or(0),
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
            _ => 0,
        });

        conn.execute(
            "INSERT OR REPLACE INTO tiddlers (title, revision, meta) VALUES (?1, ?2, ?3)",
            rusqlite::params![title, revision, tiddler_obj],
        ).map_err(|e| format!("写入条目 '{}' 失败: {}", title, e))?;
        insert_count += 1;
    }

    println!("✅ HTML 导入完成: {}", input);
    println!("   导入: {}, 跳过: {}", insert_count, skip_count);
    Ok(())
}

fn cmd_import_folder(config_path: &std::path::Path, action: &ImportAction) -> Result<(), String> {
    let cfg = load_cli_config(config_path)?;
    let db_path = cfg.server.db_path;

    let ImportAction::Folder { input } = action else { unreachable!() };

    let dir = std::path::Path::new(input);
    if !dir.is_dir() {
        return Err(format!("'{}' 不是有效目录", input));
    }

    let entries = fs::read_dir(dir)
        .map_err(|e| format!("读取目录失败: {}", e))?;

    let conn = Connection::open(&db_path)
        .map_err(|e| format!("无法打开数据库 {:?}: {}", db_path, e))?;

    let mut insert_count = 0usize;
    let mut skip_count = 0usize;

    for entry in entries {
        let entry = entry.map_err(|e| format!("读取条目失败: {}", e))?;
        let path = entry.path();
        if path.extension().map(|e| e == "tid").unwrap_or(false) {
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("读取 '{}' 失败: {}", path.display(), e))?;

            // 解析 .tid 文件：header 行直到空行，剩余为 body
            let mut headers: Vec<(&str, &str)> = Vec::new();
            let mut body = "";
            if let Some(blank_pos) = content.find("\n\n") {
                let header_section = &content[..blank_pos];
                body = content[blank_pos + 2..].trim();
                for line in header_section.lines() {
                    if let Some(pos) = line.find(':') {
                        let key = line[..pos].trim();
                        let val = line[pos + 1..].trim();
                        headers.push((key, val));
                    }
                }
            }

            // 构建 tiddler JSON
            let mut map = serde_json::Map::new();
            let mut title = String::new();
            for (key, val) in &headers {
                let k = key.to_lowercase();
                if k == "title" {
                    title = val.to_string();
                }
                map.insert(k.clone(), serde_json::Value::String(val.to_string()));
            }
            if title.is_empty() {
                skip_count += 1;
                continue;
            }
            if !body.is_empty() {
                map.insert("text".to_string(), serde_json::Value::String(body.to_string()));
            }
            map.insert("revision".to_string(), serde_json::Value::Number(serde_json::Number::from(0u64)));

            let tiddler_json = serde_json::Value::Object(map);
            conn.execute(
                "INSERT OR REPLACE INTO tiddlers (title, revision, meta) VALUES (?1, ?2, ?3)",
                rusqlite::params![title, 0u64, tiddler_json],
            ).map_err(|e| format!("写入条目 '{}' 失败: {}", title, e))?;
            insert_count += 1;
        }
    }

    println!("✅ 文件夹导入完成: {}", input);
    println!("   导入: {}, 跳过: {}", insert_count, skip_count);
    Ok(())
}
// ─── 主入口 ────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let client = WikiClient::from_env();

    let result = match &cli.command {
        Commands::Search { .. } => cmd_search(&client, &cli.command),
        Commands::Get { .. } => cmd_get(&client, &cli.command),
        Commands::Put { .. } => cmd_put(&client, &cli.command),
        Commands::Inbox { .. } => cmd_inbox(&client, &cli.command),
        Commands::List { .. } => cmd_list(&client, &cli.command),
        Commands::Delete { .. } => cmd_delete(&client, &cli.command),
        Commands::Links { .. } => cmd_links(&client, &cli.command),
        Commands::Backlinks { .. } => cmd_backlinks(&client, &cli.command),
        Commands::BatchLinks { .. } => cmd_batch_links(&client, &cli.command),
        Commands::Tags { .. } => cmd_tags(&client, &cli.command),
        Commands::Changes { .. } => cmd_changes(&client, &cli.command),
        Commands::Graph { .. } => cmd_graph(&client, &cli.command),
        Commands::Backup { config, action } => cmd_backup(config.as_path(), action),
        Commands::Export { config, action } => {
            match action {
                ExportAction::Html { .. } => cmd_export_html(config.as_path(), action),
                ExportAction::Folder { .. } => cmd_export_folder(config.as_path(), action),
            }
        }
        Commands::Import { config, action } => {
            match action {
                ImportAction::Html { .. } => cmd_import_html(config.as_path(), action),
                ImportAction::Folder { .. } => cmd_import_folder(config.as_path(), action),
            }
        }
    };

    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("wiki: {}", e);
            std::process::exit(1);
        }
    }
}
