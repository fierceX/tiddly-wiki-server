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

## 开发指南：修改内置插件

本项目内嵌了一个自定义的 TiddlyWiki 插件 (`s3-uploader`) 来处理拖拽上传逻辑。如果你需要修改上传器的 JavaScript 逻辑或 HTML 界面：

1.  进入 `s3_uploader/` 目录。
2.  直接编辑 `s3-uploader.js` (逻辑) 或 `ui-modal.html` (界面)。
3.  **重新打包插件** (使用项目自带的工具):

    ```sh
    # 从源码文件重新生成 s3_uploader_plugin.json
    cargo run --bin pack_plugin -- ./s3_uploader/manifest.json ./s3_uploader_plugin.json
    ```

4.  重新编译服务端 (`cargo build`) 以内嵌最新的插件代码。

## 许可证

本项目基于 [The Prosperity Public License 3.0.0] 许可证发布。

## 贡献

欢迎提交 Pull Request。对于重大更改，请先提交 Issue 进行讨论。

## 行为准则 (Code of Conduct)

贡献者需遵守 [Contributor Covenant](https://www.contributor-covenant.org/)。