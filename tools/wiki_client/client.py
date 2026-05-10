"""
Wiki Client 核心模块 — 封装 TiddlyWiki 服务器 API。

提供 WikiClient 类，支持:
    - search / get / put / inbox / list / delete
    - 自动认证（环境变量或显式传入）
    - URL 编码自动处理
    - 结构化错误（WikiClientError 异常，不直接 exit）
"""

import os
from datetime import datetime, timezone
from typing import Optional
from urllib.parse import quote

try:
    import requests
except ImportError:
    raise ImportError("需要安装 requests 库: pip install requests")


class WikiClientError(Exception):
    """Wiki 客户端错误（HTTP 4xx/5xx 或连接失败）。"""
    def __init__(self, status_code: int, message: str):
        self.status_code = status_code
        self.message = message
        super().__init__(f"HTTP {status_code} — {message}")


class WikiClient:
    """TiddlyWiki 服务器的 HTTP 客户端。

    参数:
        base_url:  服务器地址（默认从 WIKI_SERVER_URL 环境变量读取，fallback http://localhost:3032）
        username:  HTTP Basic Auth 用户名（默认从 WIKI_USERNAME 读取）
        password:  HTTP Basic Auth 密码（默认从 WIKI_PASSWORD 读取）
    """

    def __init__(
        self,
        base_url: Optional[str] = None,
        username: Optional[str] = None,
        password: Optional[str] = None,
    ):
        self.base_url = (
            base_url
            or os.environ.get("WIKI_SERVER_URL", "http://localhost:3032")
        ).rstrip("/")
        u = username or os.environ.get("WIKI_USERNAME", "")
        p = password or os.environ.get("WIKI_PASSWORD", "")
        self.auth = (u, p) if u and p else None
        self.session = requests.Session()
        if self.auth:
            self.session.auth = self.auth

    # ── 底层 HTTP ──────────────────────────────────────────────────

    def _get(self, path: str, params: Optional[dict] = None) -> dict:
        resp = self.session.get(f"{self.base_url}{path}", params=params)
        self._check(resp)
        return resp.json() if resp.text else {}

    def _post(self, path: str, data: dict) -> dict:
        resp = self.session.post(f"{self.base_url}{path}", json=data)
        self._check(resp)
        return resp.json() if resp.text else {}

    def _put(self, path: str, data: dict) -> int:
        resp = self.session.put(f"{self.base_url}{path}", json=data)
        self._check(resp)
        return resp.status_code

    def _delete(self, path: str) -> int:
        resp = self.session.delete(f"{self.base_url}{path}")
        self._check(resp)
        return resp.status_code

    @staticmethod
    def _check(resp: requests.Response):
        """检查响应：>= 400 时抛出 WikiClientError。"""
        if resp.status_code >= 400:
            try:
                body = resp.json()
                msg = body.get("message", resp.text)
            except Exception:
                msg = resp.text
            raise WikiClientError(resp.status_code, msg)

    # ── 高级操作 ──────────────────────────────────────────────────

    def search(
        self,
        query: str,
        tag: Optional[str] = None,
        item_type: Optional[str] = None,
        full: bool = False,
        limit: int = 20,
        offset: int = 0,
        mode: Optional[str] = None,
    ) -> list:
        """搜索条目。

        参数:
            query:     搜索关键词（匹配标题 + 正文）
            tag:       按标签过滤
            item_type: 按 item_type 字段过滤
            full:      是否返回全文（默认只返回元数据）
            limit:     每页条数（默认 20）
            offset:    偏移量
            mode:      搜索模式 — fts(默认) | regex
        """
        params: dict = {"q": query, "limit": limit, "offset": offset}
        if tag:
            params["tag"] = tag
        if item_type:
            params["item_type"] = item_type
        if full:
            params["include_text"] = "true"
        if mode:
            params["mode"] = mode
        return self._get("/api/search", params)

    def get(self, title: str) -> dict:
        """获取单个条目（通过查询参数，无需手动 URL 编码中文标题）。"""
        return self._get("/api/tiddlers", {"title": title})

    def put(
        self,
        title: str,
        content: str,
        tags: Optional[str] = None,
        item_type: str = "note",
    ) -> bool:
        """创建或更新条目。返回 True 表示成功。

        注意：会自动获取当前 revision 号以支持幂等更新。
        """
        try:
            existing = self.get(title)
            current_revision = existing.get("revision", "0")
            if isinstance(current_revision, str):
                current_revision = int(current_revision)
            is_new = False
            existing_created = existing.get("created", "")
            existing_creator = existing.get("creator", "")
        except WikiClientError:
            current_revision = 0
            is_new = True
            existing_created = ""
            existing_creator = ""

        now = datetime.now()
        ts = now.strftime("%Y%m%d%H%M%S") + f"{now.microsecond // 1000:03d}"
        username = self.auth[0] if self.auth else ""

        payload = {
            "title": title,
            "text": content,
            "type": "text/markdown",
            "tags": tags or "",
            "revision": str(current_revision),
            "modified": ts,
            "modifier": username,
        }
        # 新建时补充创建元数据；更新时保留原有的
        if is_new:
            payload["created"] = ts
            payload["creator"] = username
        else:
            if existing_created:
                payload["created"] = existing_created
            if existing_creator:
                payload["creator"] = existing_creator

        status = self._put(f"/recipes/default/tiddlers/{quote(title)}", payload)
        return status == 204

    def inbox(
        self,
        title: str,
        content: str,
        tags: Optional[list] = None,
        item_type: str = "note",
        context: Optional[str] = None,
    ) -> dict:
        """快速采集到 Inbox（带 Inbox 标签）。

        参数:
            title:   条目标题
            content: 正文
            tags:    标签列表（自动添加 "Inbox"）
            item_type: 业务类型 (note/observation/conclusion/...)
            context: 上下文（格式化为 Markdown blockquote）
        """
        payload: dict = {
            "title": title,
            "content": content,
            "tags": tags or [],
            "type": item_type,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }
        if context:
            payload["context"] = context
        return self._post("/api/inbox", payload)

    def list(self, tag: Optional[str] = None, limit: int = 50) -> list:
        """列出条目（可按标签过滤）。"""
        if tag:
            return self._get(f"/api/tiddlers/tag/{quote(tag)}", {"limit": limit})
        return self._get("/recipes/default/tiddlers.json")

    def list_inbox(self) -> list:
        """列出所有 Inbox 条目。"""
        return self._get("/api/inbox")

    def tags(self) -> list:
        """获取所有标签及其出现次数。

        返回: [{"tag": "认知", "count": 41}, {"tag": "矛盾", "count": 9}, ...]
        """
        return self._get("/api/tags")

    def links(self, title: str) -> list:
        """正向链接：列出某个条目链接了哪些目标条目标题。"""
        return self._get(f"/api/tiddlers/{quote(title, safe='')}/links")

    def backlinks(self, title: str) -> list:
        """反向链接：列出哪些条目链接到了某个目标。"""
        return self._get(f"/api/tiddlers/{quote(title, safe='')}/backlinks")

    def delete(self, title: str) -> bool:
        """删除条目。返回 True 表示成功。"""
        status = self._delete(f"/bags/default/tiddlers/{quote(title)}")
        return status == 204
