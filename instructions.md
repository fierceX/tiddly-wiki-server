# TiddlyWiki Server — Agent 操作指南

> 这是一个 Rust 重写的 TiddlyWiki 服务端，提供高效的 Wiki 托管、EPUB 阅读、S3 存储和 Agent 友好 API。

## 快速开始

启动服务器：
```bash
cargo run --release -- --config config.toml
```

## Agent 工具链

本仓库提供完整的 Python 库和 CLI 工具供 AI Agent 使用：

### Python 库

位于 `tools/wiki_client/`，封装了搜索、获取、创建、更新、Inbox 采集、删除等全部操作。

### CLI 工具

位于 `tools/wiki_cli.py`，提供与 Python 库相同的操作能力。

### 详细文档

- `SKILL.md` — Agent 技能完整参考（推荐阅读）
- `README_CN.md` — 中文用户文档（含 API 端点表）
- `README.md` — 英文用户文档

## 项目结构

```
src/main.rs          ← Rust 服务端主程序
tools/
  wiki_client/       ← Python 库源码
    client.py        ← WikiClient 类
  wiki_cli.py        ← 命令行工具
SKILL.md             ← Agent 技能文件（本文件所在目录的入口）
```

## 文件树

```
FILE: Cargo.toml
FILE: Cargo.lock
FILE: README.md
FILE: README_CN.md
FILE: SKILL.md
FILE: config.toml
DIR: src/
  FILE: main.rs
  FILE: init.sql
DIR: tools/
  FILE: wiki_cli.py
  DIR: wiki_client/
    FILE: __init__.py
    FILE: client.py
DIR: data/
  FILE: tiddlers.sqlite3
DIR: web/
  DIR: foliate-js/
    FILE: ebook_reader/reader.html
DIR: s3_uploader/
  FILE: manifest.json
  FILE: s3-uploader.js
  FILE: ui-modal.html
DIR: epub_plugin/
  FILE: manifest.json
  DIR: tiddlers/
```
