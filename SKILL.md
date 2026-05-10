# wiki-client Skill — TiddlyWiki Agent 操作指南

## 概述

本技能提供对 TiddlyWiki 服务器的 Python 库和 CLI 工具的操作能力。
仓库中包含完整的封装层，AI Agent 可通过库或 CLI 高效读写 Wiki 条目。

## 文件结构

```
tools/
  wiki_client/
    __init__.py        # 库入口
    client.py          # WikiClient 类 (search/get/put/inbox/list/delete/links/backlinks)
  wiki_cli.py          # 命令行工具 (wiki search/get/put/inbox/list/delete/links/backlinks)
```

## 依赖

```bash
pip install requests
```

## 初始化

```python
from wiki_client import WikiClient

# 方式 A：显式传参
wiki = WikiClient(
    base_url="http://localhost:3032",
    username="admin",
    password="change_me_please",
)

# 方式 B：环境变量（推荐脚本使用）
# export WIKI_SERVER_URL=http://localhost:3032
# export WIKI_USERNAME=admin
# export WIKI_PASSWORD=change_me_please
```

## 可用操作

### 搜索条目

```python
# FTS 模式（jieba 中文分词 + 前缀匹配）
wiki.search("天气", mode="fts", limit=5)
wiki.search("天气", tag="Inbox", full=True)

# 正则模式（精确模式匹配）
wiki.search(r"observ\w+tion", mode="regex")
wiki.search(r"\d{4}-\d{2}-\d{2}", mode="regex")
```

**搜索模式对比：**

| 模式 | 后端 | 性能 | 适用场景 |
|---|---|---|---|
| `fts`（默认） | SQLite FTS5 + jieba 分词 + 前缀通配符 | O(log N) | 日常关键词搜索、中文短语 |
| `regex` | 全量加载 + Rust regex 匹配 | O(N) | Agent 精确模式匹配 |

### 获取条目

```python
tiddler = wiki.get("条目标题")
# 返回 dict: {title, text, tags, created, modified, type, ...}

# 仅获取正文
text = wiki.get("条目标题")["text"]
```

### 创建/更新条目

```python
# 新建
wiki.put("新条目", content="# Markdown 正文", tags="标签1,标签2")

# 更新（会自动获取当前 revision 做幂等更新）
wiki.put("新条目", content="更新后的内容", tags="标签1,标签2")

# 指定 item_type
wiki.put("观察记录", content="内容", tags="成长记录", item_type="observation")
```

**注意：** `put` 自动处理 revision 号，无需手动管理冲突。

### 快速采集到 Inbox

```python
wiki.inbox("速记标题", content="手机发送的内容", tags=["idea", "mobile"])
wiki.inbox("观察记录", content="正文", tags=["育儿", "成长"], item_type="observation", context="来自日常观察")
```

### 列出条目

```python
# 所有条目
all_items = wiki.list(limit=50)

# 按标签过滤
inbox_items = wiki.list(tag="Inbox", limit=10)

# 列出 Inbox（专用端点）
inbox = wiki.list_inbox()
```

### 删除条目

```python
wiki.delete("要删除的标题")   # 返回 True/False
```

### 正向/反向链接

```python
# 列出某条目链接的所有目标
links = wiki.links("矛盾的概念")      # → ["矛盾的普遍性", ...]

# 列出链接到某条目的所有来源
backlinks = wiki.backlinks("矛盾的概念")  # → ["科学的研究事物和问题的方法", ...]
```

**说明：** 链接关系在写入条目时自动解析 `[[标题]]` 语法并建索引，无需手动管理。返回结果基于 `tiddler_links` 表的 O(log N) 索引查询。初始数据在服务端首次启动时自动全量回填。

## CLI 用法

```bash
# 搜索
python3 tools/wiki_cli.py search "关键词" --full --limit 5 --mode regex

# 获取
python3 tools/wiki_cli.py get "标题" --text-only

# 创建/更新
python3 tools/wiki_cli.py put "新条目" --content "正文" --tags "标签1,标签2"

# 快速采集
python3 tools/wiki_cli.py inbox "速记" --content "内容" --tags "idea,mobile"

# 列出
python3 tools/wiki_cli.py list --tag Inbox --limit 10 --plain

# 删除
python3 tools/wiki_cli.py delete "标题" --force

# 正向/反向链接
python3 tools/wiki_cli.py links "条目标题"
python3 tools/wiki_cli.py links "条目标题" --plain
python3 tools/wiki_cli.py backlinks "条目标题"
python3 tools/wiki_cli.py backlinks "条目标题" --plain
```

## 错误处理

所有操作在 HTTP 4xx/5xx 时抛出 `WikiClientError(status_code, message)` 异常。
库本身不会直接调用 `exit()`，适合 Agent 在 try/except 中安全使用。

```python
from wiki_client import WikiClientError

try:
    wiki.get("不存在的标题")
except WikiClientError as e:
    print(f"HTTP {e.status_code}: {e.message}")
```

## REST API 端点参考

| 端点 | 方法 | 用途 |
|---|---|---|
| `/api/search?q=...&mode=fts&tag=...&limit=20` | GET | 搜索 |
| `/api/tiddlers?title=xxx` | GET | 获取条目 |
| `/recipes/default/tiddlers.json` | GET | 列表 |
| `/api/tiddlers/tag/{tag}` | GET | 按标签列表 |
| `/api/tiddlers/{title}/links` | GET | 正向链接列表 |
| `/api/tiddlers/{title}/backlinks` | GET | 反向链接列表 |
| `/recipes/default/tiddlers/{title}` | PUT | 创建/更新 |
| `/api/inbox` | POST | Inbox 采集 |
| `/bags/default/tiddlers/{title}` | DELETE | 删除 |
