# TiddlyWiki Server (Rust 增强版)

[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](code_of_conduct.md)

这是一个高效、低维护成本且功能丰富的 [TiddlyWiki] Web 服务器。它是原版 [tiddly-wiki-server](https://github.com/nknight/tiddly-wiki-server) 的 Rust 重构增强版，旨在提供更好的性能、文件管理、云存储集成以及便捷的数据采集体验。

它利用 [TiddlyWeb plugin] 提供的 [Web Server API]，将条目（Tiddlers）保存在 [SQLite 数据库] 中，同时支持将大文件剥离存储到本地文件系统或 S3 云存储中。

[TiddlyWiki]: https://tiddlywiki.com/
[web server API]: https://tiddlywiki.com/#WebServer
[SQLite database]: https://sqlite.org/index.html
[TiddlyWeb plugin]: https://github.com/Jermolene/TiddlyWiki5/tree/master/plugins/tiddlywiki/tiddlyweb

## 主要改进与特性

相比原版实现，本分支包含以下重大改进：

1.  **优化的 Wiki 渲染机制**:
    - 采用代码内分割 `empty.html` 模板的方式，动态注入数据库中的条目。
    - 修复了原版中某些嵌入式插件无法正常加载或运行的问题。

2.  **本地文件剥离 (File Offloading)**:
    - 图片、PDF 等二进制文件**不再以 Base64 形式存储在 SQLite 数据库中**。
    - 文件会自动保存到本地的 `files/` 文件夹，并在 Tiddler 中通过 `_canonical_uri` 引用。这极大地减小了数据库体积并提升了加载速度。

3.  **S3/R2 直接上传支持**:
    - 支持 S3 兼容的对象存储（如 AWS S3, Cloudflare R2）。
    - **前端直传**: 服务器仅负责生成预签名 URL (Presigned URL)，浏览器直接将文件上传至云存储。这不仅减轻了服务器带宽压力，还提高了上传稳定性。

4.  **基础鉴权 (Basic Authentication)**:
    - 内置 HTTP Basic Auth 中间件。
    - 在公网部署时，可通过配置账号密码保护您的 Wiki 不被未授权访问。

5.  **快速采集 (Inbox) API**:
    - 专为移动端自动化设计的 Webhook 接口 (`/api/inbox`)。
    - 可轻松集成 **iOS 快捷指令**、**Android HTTP Shortcuts** 无需加载完整网页即可随时记录灵感。

## 配置文件说明

请在运行目录下创建 `config.toml` 文件：

> **安全提示**: 当启用 Basic Auth (`[auth]`) 时，强烈建议配合反向代理（如 Nginx/Caddy）并开启 **HTTPS** 使用，因为密码是通过 Base64 编码明文传输的。

```toml
[server]
bind = "0.0.0.0"              # 监听地址
port = 3032                   # 监听端口
db_path = "./data/tiddlers.sqlite3" # 数据库路径
files_dir = "./files/"        # 本地文件存储路径

[status]
username = "FierceX"          # Wiki 中显示的用户名

# [可选] HTTP 基础鉴权
# 如果注释掉此部分，服务器将无密码运行
[auth]
username = "admin"
password = "change_me_please"

[s3]
enable = true                 # 是否启用 S3 上传
access_key = "YOUR_ACCESS_KEY"
secret_key = "YOUR_SECRET_KEY"
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
region = "auto"
bucket_name = "your-bucket"
public_url_base = "https://your-public-domain.com" # 文件的公开访问前缀
```

## 快速采集 API (Inbox)

本服务器暴露了一个轻量级的 Webhook 接口，用于从外部工具快速捕捉想法。

- **接口地址**: `POST /api/inbox`
- **鉴权**: 需要 HTTP Basic Auth（如果配置中启用了的话）。
- **Content-Type**: `application/json`

### JSON 数据格式

```json
{
  "text": "这是一条来自手机的灵感记录。",
  "tags": "idea mobile" 
}
```
*`tags` 是可选的。如果省略，默认标签为 "Inbox"。*

### 集成示例

#### 1. curl (命令行)
```bash
curl -X POST https://your-wiki.com/api/inbox \
     -u "admin:change_me_please" \
     -H "Content-Type: application/json" \
     -d '{"text": "来自终端的问候！", "tags": "cli"}'
```

#### 2. iOS 快捷指令 / Android HTTP Shortcuts
在您的快捷指令 App 中配置以下参数：
*   **URL**: `https://your-wiki.com/api/inbox`
*   **方法 (Method)**: `POST`
*   **头部 (Headers)**: 
    *   `Authorization`: `Basic <Base64编码的账号密码>` (例如: `Basic YWRtaW46MTIzNDU2`)
*   **请求体 (Body)**: JSON
    *   `text`: (选择 "每次询问" 或 "剪贴板")

采集到的条目将出现在您的 Wiki 中，带有 `Inbox` 标签和以时间戳命名的标题。

## 运行服务器

### 手动编译运行

1.  **编译**:
    ```sh
    cargo build --release
    ```
2.  **运行**:
    确保 `config.toml` 和 `empty.html` 在当前或指定路径下。
    ```sh
    ./target/release/tiddly-wiki-server --config config.toml
    ```

## 项目初衷

TiddlyWiki 5 官方的 [NodeJS server] 虽然兼容性极佳，但资源占用较高（通常需要 70MB+ 内存）。这个 Rust 版本旨在以极低的资源占用（约 10MB 内存）提供相同的功能，并增加了 S3 直传、移动端快速采集等现代功能，使其更适合在廉价 VPS 或个人服务器上部署。

[NodeJS server]: https://tiddlywiki.com/static/WebServer.html

## 许可证

本项目基于 [The Prosperity Public License 3.0.0] 授权。

## 贡献

欢迎提交 Pull Request。如果是重大修改，请先提交 Issue 讨论。