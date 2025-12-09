# TiddlyWiki Server (Rust Enhanced)

[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](code_of_conduct.md)  

[简体中文](./README_CN.md)  

This is an efficient, low-maintenance, and feature-rich web server for [TiddlyWiki]. It is a fork of the original [tiddly-wiki-server](https://github.com/nknight/tiddly-wiki-server), rewritten in Rust to provide better performance, file management, and cloud integration.

It uses the [web server API] provided by the [TiddlyWeb plugin] to save tiddlers in a [SQLite database], while offloading binary files to local storage or S3-compatible cloud storage.

[TiddlyWiki]: https://tiddlywiki.com/
[web server API]: https://tiddlywiki.com/#WebServer
[SQLite database]: https://sqlite.org/index.html
[TiddlyWeb plugin]: https://github.com/Jermolene/TiddlyWiki5/tree/master/plugins/tiddlywiki/tiddlyweb

## Key Improvements & Features

Compared to the original implementation, this fork includes significant enhancements:

1.  **Optimized Wiki Rendering**: 
    - The server now dynamically injects tiddlers into `empty.html` by splitting the template in memory. 
    - Fixed issues where embedded plugins were not loading correctly in the original implementation.
    
2.  **Local File Offloading**:
    - Binary files (images, PDFs, etc.) are **no longer stored as Base64 strings** inside the SQLite database.
    - They are automatically saved to a local `files/` directory, and the Tiddler is updated with a `_canonical_uri` pointer. This keeps the database small and the wiki fast.

3.  **S3/R2 Direct Upload Support**:
    - Supports S3-compatible storage (e.g., AWS S3, Cloudflare R2).
    - **Direct Upload**: The server generates a pre-signed PUT URL. The browser uploads the file directly to the cloud storage. Bandwidth is saved as files do not pass through the application server.

4.  **Configuration & Logging**:
    - Replaced command-line flags with a structured `config.toml` file.
    - Configurable **Username** (displayed in the Wiki status).
    - Integrated `tracing` for structured logging (configurable via `RUST_LOG`).

## Configuration

Create a `config.toml` file in the working directory.

```toml
[server]
bind = "0.0.0.0"
port = 3032
db_path = "./data/tiddlers.sqlite3"
files_dir = "./files/"

[status]
username = "YourName" 

[s3]
enable = true
access_key = "YOUR_ACCESS_KEY"
secret_key = "YOUR_SECRET_KEY"
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
region = "auto"
bucket_name = "your-bucket"
public_url_base = "https://your-public-domain.com"
```

## Running the Server

### Manual Installation

1.  **Build**:
    ```sh
    cargo build --release
    ```
2.  **Run**:
    Ensure `config.toml` and `empty.html` are in the correct path.
    ```sh
    ./target/release/tiddly-wiki-server --config config.toml
    ```

## Motivation

TiddlyWiki 5 has a [NodeJS based web server] that is excellent but resource-heavy (often requiring 70MB+ RAM). This Rust implementation aims to provide the same functionality with a fraction of the footprint (approx. 10MB RAM), while adding modern features like S3 offloading which are typically complex to configure in the standard NodeJS version.

[NodeJS based web server]: https://tiddlywiki.com/static/WebServer.html

## License

This project is made available under [The Prosperity Public License 3.0.0].

## Contributing

Pull requests are welcome. For major changes, please open an issue first to discuss what you would like to change.

## Code of Conduct

Contributors are expected to abide by the [Contributor Covenant](https://www.contributor-covenant.org/).
