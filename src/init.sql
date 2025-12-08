CREATE TABLE IF NOT EXISTS tiddlers 
(
    title TEXT UNIQUE PRIMARY KEY,
    revision INTEGER,
    meta BLOB
);
CREATE INDEX IF NOT EXISTS tiddlers_title_index ON tiddlers (title);
INSERT INTO tiddlers (title, revision, meta) VALUES (
    '$:/config/CPL-Source',
    0,
    '{
        "title": "$:/config/CPL-Source",
        "tags": ["$:/tags/PluginLibrary"],
        "caption": "CPL 中文社区插件源",
        "url": "https://tiddly-gittly.github.io/TiddlyWiki-CPL/library/index.html",
        "type": "text/vnd.tiddlywiki"
    }'
)
ON CONFLICT(title) DO UPDATE SET
    meta = excluded.meta,
    revision = revision + 1;