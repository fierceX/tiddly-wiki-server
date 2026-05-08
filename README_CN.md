# TiddlyWiki 服务端 (Rust 增强版)

[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](code_of_conduct.md)
![Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange)
![License](https://img.shields.io/badge/license-Prosperity%20Public-blue)

[English](./README.md)

这是一个为 [TiddlyWiki] 设计的高效、低维护且功能丰富的 Web 服务端。它是原版 [tiddly-wiki-server](https://github.com/nknight/tiddly-wiki-server) 的 Rust 重写版，旨在提供更强的性能、更完善的文件管理、云存储集成以及快速采集能力。

该服务端利用 [TiddlyWeb 插件] 提供的 [Web Server API]，将条目（Tiddlers）保存在 [SQLite 数据库] 中，同时能够智能地将图片、PDF 等二进制文件分离存储到本地磁盘或兼容 S3 的云存储中。

[TiddlyWiki]: https://tiddlywiki.com/
[Web Server API]: https://tiddlywiki.com/#WebServer
[SQLite 数据库]: https://sqlite.org/index.html
[TiddlyWeb 插件]: https://github.com/Jermolene/TiddlyWiki5/tree/master/plugins/tiddlywiki/tiddlyweb

## 核心改进与特性

与原版实现相比，本分支包含了以下重大改进：

### 🚀 性能与渲染
-   **优化的 Wiki 渲染**：通过高效的内存拆分技术，将条目动态注入到 `empty.html` 模板中，大幅提升加载速度。
-   **极低资源占用**：运行时仅需约 10MB 内存，而标准的 NodeJS 版服务端通常需要 70MB+。

### 📖 集成 EPUB 阅读器
-   **直接阅读**：服务端二进制文件直接内嵌了自定义的 [Foliate-js](https://github.com/johnfactotum/foliate-js) 阅读器。
-   **无缝体验**：自动检测 `application/epub+zip` 类型的条目（通过 `_canonical_uri` 引用），并在现代化的阅读界面中渲染，替换默认的“二进制文件”下载提示。
-   **S3 & 本地支持**：支持直接从 S3 或本地存储流式传输 EPUB 文件，无需先完整下载。
-   **关于 CSP 的说明**：此内嵌阅读器**不会**执行客户端的内容安全策略 (CSP) 清洗。如果你导入的是由 Readeck 等工具生成且注入了严格 CSP meta 标签的 EPUB 文件，请确保在上传前对文件进行清洗/去毒。

### ☁️ 智能存储 (S3 & 本地)
-   **本地文件分离**：二进制文件（图片、PDF 等）不再以 Base64 字符串形式存入数据库，而是自动保存到 `files/` 目录。Tiddler 仅保留 `_canonical_uri` 引用，确保数据库轻量且 Wiki 运行流畅。
-   **S3/R2 直传支持**：
    -   服务端生成预签名 URL (Pre-signed URL)，浏览器直接将文件上传至对象存储。
    -   节省服务器带宽，支持大文件上传，无需经过应用服务器中转。
-   **基于元数据的健壮性**：Tiddler 内部字段（`_s3_key`, `_s3_bucket` 等）直接记录了文件的存储元数据。这意味着即使服务器配置变更（如更换 Bucket），旧文件的管理和删除依然准确无误。
-   **级联删除 (Cascade Delete)**：当你在 Wiki 中删除一个条目时，服务端会**自动清理** S3 上或本地磁盘对应的文件。彻底告别“孤儿文件”和存储垃圾。

### 🔒 安全与认证
-   **基础认证 (Basic Auth)**：内置 HTTP Basic Auth 中间件，保护部署在公网的 Wiki 不被未授权访问。
-   **API 鉴权**：支持标准的 `Authorization` 请求头，方便第三方工具集成。

### 📥 快速采集 (Inbox)
-   提供专用的 Webhook 端点 (`/api/inbox`)，专为移动端自动化设计。
-   轻松集成 **iOS 快捷指令 (Shortcuts)** 或 **Android HTTP Shortcuts**，无需加载完整的 Wiki 界面即可瞬间捕捉灵感。

## 配置指南

在工作目录下创建一个 `config.toml` 文件。

> **安全提示**：如果启用了基础认证 (`[auth]`)，强烈建议配合反向代理（如 Nginx/Caddy）并开启 **HTTPS**，因为密码是以 Base64 编码传输的。

```toml
[server]
bind = "0.0.0.0"
port = 3032
db_path = "./data/tiddlers.sqlite3"  # 数据库存储路径
files_dir = "./files/"               # 本地文件存储路径

# 在 Wiki 修订记录中显示的用户名
[status]
username = "YourName" 

# [可选] HTTP 基础认证
# 如果注释掉此部分，服务器将允许匿名访问
[auth]
username = "admin"
password = "change_me_please"

[s3]
enable = true
name = "r2"
access_key = "YOUR_AWS_ACCESS_KEY"
secret_key = "YOUR_AWS_SECRET_KEY"
# 示例：Cloudflare R2 的 endpoint
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
region = "auto"
bucket_name = "your-wiki-assets"
# 你的资源公开访问域名
public_url_base = "https://assets.your-domain.com"
```

## 快速采集 API (Inbox)

无需打开 Wiki 即可从外部工具快速保存内容。

-   **端点**: `POST /api/inbox`
-   **Content-Type**: `application/json`

### JSON 数据格式

```json
{
  "text": "这是一条从手机发送的速记。",
  "tags": "idea mobile" 
}
```
*`tags` 是可选的。如果省略，默认标签为 "Inbox"。*

### iOS / Android 快捷指令示例
*   **URL**: `https://your-wiki.com/api/inbox`
*   **方法**: `POST`
*   **头部 (Headers)**: `Authorization: Basic <Base64编码的账号密码>`
*   **请求体 (Body)**: JSON (将剪贴板内容或输入文本作为 `text` 字段发送)

采集的内容将作为一个带有时间戳标题的新条目出现在 Wiki 中，并带有 `Inbox` 标签。

## 安装与运行

1.  **编译**:
    ```sh
    cargo build --release
    ```
2.  **运行**:
    确保 `config.toml` 和 `empty.html` 位于当前目录中。
    ```sh
    ./target/release/tiddly-wiki-server --config config.toml
    ```

## 开发指南：插件与静态资源

本服务端内嵌了自定义的 TiddlyWiki 插件和静态资源（如阅读器）。你可以按照以下方式进行修改：

### 1. S3 上传插件 (S3 Uploader)
处理拖拽上传至 S3/本地存储的逻辑。
*   **源码位置**: `s3_uploader/`
*   **打包命令**:
    ```sh
    cargo run --bin pack_plugin -- ./s3_uploader/manifest.json ./s3_uploader_plugin.json
    ```

### 2. EPUB 阅读器插件 (EPUB Viewer Plugin)
处理 TiddlyWiki UI 与 EPUB 文件的集成逻辑。
*   **源码位置**: `epub_plugin/`
*   **打包命令**:
    ```sh
    cargo run --bin pack_plugin -- ./epub_plugin/manifest.json ./epub_viewer_plugin.json
    ```

### 3. Foliate 阅读器 (内嵌静态资源)
实际的阅读引擎是 Foliate-js 的定制构建版，直接嵌入在服务端二进制文件中。
*   **源码位置**: `web/foliate-js/` (包含修改后的源代码)
*   **构建产物**: `web/foliate-js/ebook_reader/` (Rust 嵌入的优化后的静态文件)
*   **如何更新**: 
    如果你修改了阅读器的源代码，必须在重新编译 Rust 服务端之前重建前端资源：
    ```sh
    cd web/foliate
    npm install
    npx vite build
    # 确保输出位于 ./ebook_reader 且包含 reader.html
    ```

修改任何插件或资源后，请运行 `cargo build` 以将新版本嵌入到服务端可执行文件中。

## Agent 友好 API 与 Python 库

本服务端提供一组专为 AI Agent 和自动化工具设计的 HTTP API，以及配套的 Python 封装库和命令行工具。

### Python 库 `wiki_client`

位于 `tools/wiki_client/`，通过 pip 安装依赖后即可使用：

```bash
pip install requests
```

#### 快速入门

```python
from wiki_client import WikiClient

wiki = WikiClient(
    base_url="http://localhost:3032",
    username="admin",
    password="change_me_please",
)

# 搜索条目（支持中文分词）
results = wiki.search("天气", mode="fts")
# → [{"title": "...", "text": "..."}]

# 正则模式搜索（Agent 友好）
results = wiki.search(r"observ\w+tion", mode="regex")

# 获取单个条目
tiddler = wiki.get("我的标题")

# 创建/更新条目
wiki.put("新条目", content="# 正文\nMarkdown 格式", tags="标签1,标签2")

# 快速采集到 Inbox
wiki.inbox("速记标题", content="手机发送的内容", tags=["idea", "mobile"])

# 列出条目（可按标签过滤）
wiki.list(tag="Inbox", limit=10)

# 删除条目
wiki.delete("要删除的标题")
```

所有方法均抛出 `WikiClientError` 异常（含 HTTP 状态码），不直接 `exit`。

#### `search()` 参数详解

```python
def search(
    query: str,
    tag: str = None,        # 按标签过滤
    item_type: str = None,  # 按 item_type 过滤 (note/observation/...)
    full: bool = False,     # 返回全部字段（默认仅元数据）
    limit: int = 20,        # 每页条数
    offset: int = 0,        # 偏移
    mode: str = None,       # 搜索模式: "fts"(默认) | "regex"
)
```

### 搜索模式

#### `mode=fts`（默认）— FTS5 + jieba 中文分词

- 使用 SQLite FTS5 全文索引，搭配 **jieba-rs** 中文分词
- 自动处理中文分词（"天气" → 匹配"天气晴朗"）
- 支持前缀通配符（"plug" → 匹配"plugin"）
- 性能 O(log N)，适合日常搜索

#### `mode=regex` — 正则表达式

- 全量加载后使用 Rust `regex` crate 匹配
- 支持完整正则语法：`\d+`, `^.*(Report|Note).*$`, `\bword\b`
- 可与 `tag` / `item_type` 参数组合过滤
- 适合 Agent 需要精确模式匹配的场景

### 命令行工具 `wiki_cli.py`

```bash
# 通过环境变量设置认证
export WIKI_SERVER_URL=http://localhost:3032
export WIKI_USERNAME=admin
export WIKI_PASSWORD=change_me_please

# 搜索
python3 tools/wiki_cli.py search "天气" --full --limit 5
python3 tools/wiki_cli.py search ".*观察.*" --mode regex --tag Inbox

# 获取
python3 tools/wiki_cli.py get "我的标题" --text-only

# 创建/更新
python3 tools/wiki_cli.py put "新条目" --content "正文" --tags "标签1,标签2"

# 快速采集
python3 tools/wiki_cli.py inbox "速记" --content "内容" --tags "idea,mobile"

# 列出
python3 tools/wiki_cli.py list --tag Inbox --limit 10
python3 tools/wiki_cli.py list --inbox --plain

# 删除
python3 tools/wiki_cli.py delete "条目标题" --force
```

### REST API 端点一览

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/search?q=关键词&mode=fts&tag=Inbox&limit=20` | GET | 搜索条目（fts/regex 模式） |
| `/api/tiddlers?title=标题` | GET | 按标题获取条目（无需 URL 编码） |
| `/recipes/default/tiddlers.json` | GET | 列出所有条目 |
| `/api/tiddlers/tag/{tag}` | GET | 按标签列出条目 |
| `/recipes/default/tiddlers/{title}` | PUT | 创建/更新条目 |
| `/api/inbox` | POST | 快速采集到 Inbox |
| `/api/inbox` | GET | 列出 Inbox 条目 |
| `/bags/default/tiddlers/{title}` | DELETE | 删除条目 |
| `/status` | GET | 服务器状态 |

## 许可证

本项目基于 [The Prosperity Public License 3.0.0] 许可证发布。

## 贡献

欢迎提交 Pull Request。对于重大更改，请先提交 Issue 进行讨论。

## 行为准则 (Code of Conduct)

贡献者需遵守 [Contributor Covenant](https://www.contributor-covenant.org/)。