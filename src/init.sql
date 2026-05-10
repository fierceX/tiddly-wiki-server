CREATE TABLE IF NOT EXISTS tiddlers 
(
    title TEXT UNIQUE PRIMARY KEY,
    revision INTEGER,
    meta BLOB
);
CREATE INDEX IF NOT EXISTS tiddlers_title_index ON tiddlers (title);

-- FTS5 全文搜索索引（Agent 友好搜索）
-- 前缀索引 1..20 支持 build_fts_query 生成的 "token*" 前缀匹配
CREATE VIRTUAL TABLE IF NOT EXISTS tiddlers_fts USING fts5(
    title,
    text,
    tags,
    prefix='1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20'
);

-- [[链接]] 索引表，用于正向/反向链接查询
CREATE TABLE IF NOT EXISTS tiddler_links (
    source TEXT NOT NULL,
    target TEXT NOT NULL,
    PRIMARY KEY (source, target)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_links_target ON tiddler_links(target);
