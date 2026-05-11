//! Wiki CLI — Rust 原生实现，无 Python 依赖
//!
//! 用法: wiki <子命令> [参数]
//!
//! 环境变量:
//!   WIKI_SERVER_URL  (默认 http://localhost:3032)
//!   WIKI_USERNAME
//!   WIKI_PASSWORD

use clap::{Parser, Subcommand};
use chrono::Utc;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
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
        let password = std::env::var("WIKI_PASSWORD").unwrap_or_default();

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

    fn put(&self, title: &str, content: &str, tags: Option<&str>, item_type: &str) -> Result<u16, String> {
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

        let now = chrono::Local::now();
        let ts = now.format("%Y%m%d%H%M%S").to_string() + &format!("{:03}", now.timestamp_subsec_millis());
        let username = std::env::var("WIKI_USERNAME").unwrap_or_default();

        let mut payload = json!({
            "title": title,
            "text": content,
            "type": "text/markdown",
            "tags": tags.unwrap_or(""),
            "revision": revision,
            "modified": ts,
            "modifier": username,
        });

        // 新建时补充创建元数据
        let is_new = revision == "0";
        if is_new {
            let map = payload.as_object_mut().unwrap();
            map.insert("created".into(), json!(ts));
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
    };

    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("wiki: {}", e);
            std::process::exit(1);
        }
    }
}
