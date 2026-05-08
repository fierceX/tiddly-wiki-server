"""
Wiki Client — Agent-friendly TiddlyWiki Python library.

用法:
    from wiki_client import WikiClient

    wiki = WikiClient()
    results = wiki.search("关键词", full=True, limit=5)
    results = wiki.search(".*关键词.*", mode="regex")   # 正则模式
    wiki.inbox("标题", content="正文", tags=["收件箱"], item_type="note")
    wiki.put("标题", content="更新内容", tags="标签")
"""

from .client import WikiClient, WikiClientError

__all__ = ["WikiClient", "WikiClientError"]
