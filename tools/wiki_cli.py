#!/usr/bin/env python3
"""
Wiki CLI — Agent-friendly TiddlyWiki command-line tool.

环境变量:
    WIKI_SERVER_URL    Wiki 服务器地址（默认 http://localhost:3032）
    WIKI_USERNAME      HTTP Basic Auth 用户名
    WIKI_PASSWORD      HTTP Basic Auth 密码

用法:
    wiki search "<关键词>" --full --limit 5
    wiki get "<标题>" --text-only
    wiki put "<标题>" --content "正文" --tags "tag1,tag2" --type note
    wiki inbox "<标题>" --content "正文" --tags "收件箱"
    wiki list --tag Inbox --limit 10
    wiki delete "<标题>" --force

依赖: pip install requests
"""

import argparse
import json
import os
import sys

# 确保能找到同目录下的 wiki_client 包
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from wiki_client import WikiClient, WikiClientError


# ── CLI 命令 ────────────────────────────────────────────────────────────

def cmd_search(args):
    try:
        client = WikiClient()
        results = client.search(
            query=args.query,
            tag=args.tag,
            item_type=getattr(args, "type", None),
            full=args.full,
            limit=args.limit,
            offset=args.offset,
            mode=getattr(args, "mode", None),
            modified_after=getattr(args, "modified_after", None),
            modified_before=getattr(args, "modified_before", None),
            created_after=getattr(args, "created_after", None),
            created_before=getattr(args, "created_before", None),
        )
        if args.plain:
            for r in results:
                print(r.get("title", ""))
        else:
            print(json.dumps(results, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_get(args):
    try:
        client = WikiClient()
        result = client.get(args.title)
        if args.text_only:
            print(result.get("text", ""))
        else:
            print(json.dumps(result, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_put(args):
    try:
        client = WikiClient()
        content = args.content or ""
        if args.file:
            with open(args.file, "r", encoding="utf-8") as f:
                content = f.read()
        if not content:
            sys.stderr.write("wiki: 需要 --content 或 --file 提供正文\n")
            sys.exit(1)
        ok = client.put(
            title=args.title,
            content=content,
            tags=args.tags,
            item_type=getattr(args, "type", "note"),
        )
        if ok:
            print(f"wiki: '{args.title}' 写入成功")
        else:
            sys.stderr.write(f"wiki: '{args.title}' 写入失败\n")
            sys.exit(1)
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_inbox(args):
    try:
        client = WikiClient()
        content = args.content or ""
        if args.file:
            with open(args.file, "r", encoding="utf-8") as f:
                content = f.read()
        if not content:
            sys.stderr.write("wiki: 需要 --content 或 --file 提供正文\n")
            sys.exit(1)
        tags = args.tags.split(",") if args.tags else []
        result = client.inbox(
            title=args.title,
            content=content,
            tags=tags,
            item_type=getattr(args, "type", "note"),
            context=args.context,
        )
        print(json.dumps(result, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_list(args):
    try:
        client = WikiClient()
        if args.inbox:
            results = client.list_inbox()
        elif args.tag:
            results = client.list(tag=args.tag, limit=args.limit)
        else:
            results = client.list(limit=args.limit)

        if args.plain:
            for r in results:
                title = r.get("title", "")
                tags = r.get("tags", "")
                if isinstance(tags, list):
                    tags = " ".join(tags)
                print(f"{title}  [{tags}]")
        else:
            print(json.dumps(results, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_delete(args):
    try:
        client = WikiClient()
        if not args.force:
            sys.stderr.write(f"wiki: 确认删除 '{args.title}'? 使用 --force 跳过确认\n")
            sys.exit(1)
        ok = client.delete(args.title)
        if ok:
            print(f"wiki: '{args.title}' 已删除")
        else:
            sys.stderr.write(f"wiki: '{args.title}' 删除失败\n")
            sys.exit(1)
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_links(args):
    """正向链接：列出某条目链接的目标。"""
    try:
        client = WikiClient()
        results = client.links(args.title)
        if args.plain:
            for t in results:
                print(t)
        else:
            print(json.dumps(results, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_backlinks(args):
    """反向链接：列出链接到某条目的来源。"""
    try:
        client = WikiClient()
        results = client.backlinks(args.title)
        if args.plain:
            for t in results:
                print(t)
        else:
            print(json.dumps(results, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_tags(args):
    """列出所有标签及出现次数。"""
    try:
        client = WikiClient()
        results = client.tags()
        if args.plain:
            for entry in results:
                print(f"{entry['tag']:30s}  {entry['count']:4d}")
        else:
            print(json.dumps(results, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_changes(args):
    """列出最近修改的条目（增量感知）。"""
    try:
        client = WikiClient()
        params = {"limit": args.limit}
        if args.since:
            # 支持自然语言: "24h", "7d", "20260501"
            if args.since.endswith("h"):
                import datetime
                ts = (datetime.datetime.now() - datetime.timedelta(hours=int(args.since[:-1])))
                params["modified_after"] = ts.strftime("%Y%m%d%H%M%S") + "000"
            elif args.since.endswith("d"):
                import datetime
                ts = (datetime.datetime.now() - datetime.timedelta(days=int(args.since[:-1])))
                params["modified_after"] = ts.strftime("%Y%m%d%H%M%S") + "000"
            else:
                params["modified_after"] = args.since
        if args.tag:
            params["tag"] = args.tag
        results = client._get("/api/search", params)
        if args.plain:
            for r in results:
                mod = r.get("modified", "")[:8]
                title = r.get("title", "")
                print(f"{mod}  {title}")
        else:
            print(json.dumps(results, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_batch_links(args):
    """批量查询多个条目的正向链接。"""
    try:
        client = WikiClient()
        titles = args.titles
        result = {}
        for t in titles:
            try:
                links = client.links(t)
                result[t] = links
            except Exception:
                result[t] = []
        if args.plain:
            for source, targets in result.items():
                if targets:
                    print(f"{source}:")
                    for tg in targets:
                        print(f"  → {tg}")
        else:
            print(json.dumps(result, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


def cmd_graph(args):
    """从指定条目出发做 BFS 遍历，输出链接图谱。"""
    try:
        client = WikiClient()
        start = args.start
        depth = args.depth
        visited = set()
        queue = [(start, 0)]
        graph = {}  # {title: [linked_titles]}
        all_user_items = None  # lazy load

        while queue:
            title, d = queue.pop(0)
            if title in visited or d > depth:
                continue
            visited.add(title)
            try:
                targets = client.links(title)
                graph[title] = targets
                if d < depth:
                    for t in targets:
                        if t not in visited:
                            queue.append((t, d + 1))
            except Exception:
                graph[title] = []

        if args.plain:
            for source, targets in graph.items():
                if targets:
                    print(f"{source}  →  {', '.join(targets)}")
                else:
                    print(f"{source}  →  (无链接)")
        else:
            print(json.dumps(graph, ensure_ascii=False, indent=2))
    except WikiClientError as e:
        sys.stderr.write(f"wiki: {e}\n")
        sys.exit(1)


# ── 主入口 ──────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        prog="wiki",
        description="Agent-friendly TiddlyWiki CLI tool",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    # search
    p = sub.add_parser("search", help="搜索条目")
    p.add_argument("query", help="搜索关键词")
    p.add_argument("--tag", help="按标签过滤")
    p.add_argument("--type", dest="type", help="按 item_type 过滤")
    p.add_argument("--full", action="store_true", help="返回全文")
    p.add_argument("--limit", type=int, default=20, help="每页条数（默认 20）")
    p.add_argument("--offset", type=int, default=0, help="偏移")
    p.add_argument("--plain", action="store_true", help="纯文本标题列表")
    p.add_argument("--mode", choices=["fts", "regex"], default=None, help="搜索模式：fts(默认) | regex")
    p.add_argument("--modified-after", help="修改时间 ≥ (YYYYMMDDHHMMSSmmm)")
    p.add_argument("--modified-before", help="修改时间 ≤")
    p.add_argument("--created-after", help="创建时间 ≥")
    p.add_argument("--created-before", help="创建时间 ≤")
    p.set_defaults(func=cmd_search)

    # get
    p = sub.add_parser("get", help="获取单个条目")
    p.add_argument("title", help="条目标题")
    p.add_argument("--text-only", action="store_true", help="只输出正文")
    p.set_defaults(func=cmd_get)

    # put
    p = sub.add_parser("put", help="创建/更新条目")
    p.add_argument("title", help="条目标题")
    p.add_argument("--content", help="正文内容")
    p.add_argument("--file", help="从文件读取正文")
    p.add_argument("--tags", help="标签（逗号分隔）")
    p.add_argument("--type", dest="type", default="note", help="条目类型（默认 note）")
    p.set_defaults(func=cmd_put)

    # inbox
    p = sub.add_parser("inbox", help="快速采集到 Inbox")
    p.add_argument("title", help="条目标题")
    p.add_argument("--content", help="正文内容")
    p.add_argument("--file", help="从文件读取正文")
    p.add_argument("--tags", help="标签（逗号分隔）")
    p.add_argument("--type", dest="type", default="note", help="条目类型（默认 note）")
    p.add_argument("--context", help="上下文说明（格式化为 Markdown blockquote）")
    p.set_defaults(func=cmd_inbox)

    # list
    p = sub.add_parser("list", help="列出条目")
    p.add_argument("--tag", help="按标签过滤")
    p.add_argument("--limit", type=int, default=50, help="最大条数")
    p.add_argument("--inbox", action="store_true", help="只列出 Inbox 条目")
    p.add_argument("--plain", action="store_true", help="纯文本列表")
    p.set_defaults(func=cmd_list)

    # delete
    p = sub.add_parser("delete", help="删除条目")
    p.add_argument("title", help="条目标题")
    p.add_argument("--force", action="store_true", help="跳过确认")
    p.set_defaults(func=cmd_delete)

    # links
    p = sub.add_parser("links", help="列出正向链接（某条目链接的目标）")
    p.add_argument("title", help="条目标题")
    p.add_argument("--plain", action="store_true", help="纯文本列表")
    p.set_defaults(func=cmd_links)

    # backlinks
    p = sub.add_parser("backlinks", help="列出反向链接（链接到某条目的来源）")
    p.add_argument("title", help="条目标题")
    p.add_argument("--plain", action="store_true", help="纯文本列表")
    p.set_defaults(func=cmd_backlinks)

    # tags
    p = sub.add_parser("tags", help="列出所有标签及出现次数")
    p.add_argument("--plain", action="store_true", help="纯文本列表")
    p.set_defaults(func=cmd_tags)

    # changes
    p = sub.add_parser("changes", help="列出最近修改的条目（增量感知）")
    p.add_argument("--since", default="24h", help="时间范围: 24h / 7d / YYYYMMDDHHMMSSmmm (默认 24h)")
    p.add_argument("--tag", help="按标签过滤")
    p.add_argument("--limit", type=int, default=50, help="最大条数")
    p.add_argument("--plain", action="store_true", help="纯文本列表")
    p.set_defaults(func=cmd_changes)

    # batch-links
    p = sub.add_parser("batch-links", help="批量查询多个条目的正向链接")
    p.add_argument("titles", nargs="+", help="条目标题（可多个）")
    p.add_argument("--plain", action="store_true", help="纯文本输出")
    p.set_defaults(func=cmd_batch_links)

    # graph
    p = sub.add_parser("graph", help="从指定条目出发 BFS 遍历链接图谱")
    p.add_argument("start", help="起始条目标题")
    p.add_argument("--depth", type=int, default=2, help="遍历深度（默认 2）")
    p.add_argument("--plain", action="store_true", help="纯文本输出")
    p.set_defaults(func=cmd_graph)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
