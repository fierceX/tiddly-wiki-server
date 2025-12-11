# TiddlyWiki Server (Rust Enhanced)

[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](code_of_conduct.md)
![Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange)
![License](https://img.shields.io/badge/license-Prosperity%20Public-blue)

[简体中文](./README_CN.md)

This is an efficient, low-maintenance, and feature-rich web server for [TiddlyWiki]. It is a fork of the original [tiddly-wiki-server](https://github.com/nknight/tiddly-wiki-server), rewritten in Rust to provide better performance, file management, cloud integration, and quick capture capabilities.

It uses the [web server API] provided by the [TiddlyWeb plugin] to save tiddlers in a [SQLite database], while smartly offloading binary files to local storage or S3-compatible cloud storage.

[TiddlyWiki]: https://tiddlywiki.com/
[web server API]: https://tiddlywiki.com/#WebServer
[SQLite database]: https://sqlite.org/index.html
[TiddlyWeb plugin]: https://github.com/Jermolene/TiddlyWiki5/tree/master/plugins/tiddlywiki/tiddlyweb

## Key Improvements & Features

Compared to the original implementation, this fork includes significant enhancements:

### 🚀 Performance & Rendering
- **Optimized Wiki Rendering**: Dynamically injects tiddlers into `empty.html` via efficient memory splitting.
- **Low Footprint**: Runs with approx. 10MB RAM, compared to 70MB+ for the standard NodeJS server.

### ☁️ Smart Storage (S3 & Local)
- **Local File Offloading**: Binary files (images, PDFs) are kept out of the SQLite database to ensure speed. They are stored in `files/`, and Tiddlers simply reference them via `_canonical_uri`.
- **S3/R2 Direct Upload**: 
    - Generates pre-signed URLs for secure, direct browser-to-cloud uploads.
    - Saves server bandwidth and supports huge files.
- **Metadata-Driven Robustness**: Tiddlers store storage metadata (Bucket, Key, Region) directly in their fields (`_s3_key`, etc.). This means file management remains accurate even if server configurations change.
- **Cascade Delete**: When you delete a Tiddler in the Wiki, the server **automatically cleans up** the corresponding file on S3 or the local disk. No more orphaned files!

### 🔒 Security & Auth
- **Basic Authentication**: Built-in HTTP Basic Auth middleware to protect your wiki on public networks.
- **Authorization Headers**: Supports standard `Authorization` headers for API integration.

### 📥 Quick Capture (Inbox)
- A specialized Webhook endpoint (`/api/inbox`) designed for mobile automation.
- Easily integrates with **iOS Shortcuts** or **Android HTTP Shortcuts** to capture thoughts instantly without loading the full interface.

## Configuration

Create a `config.toml` file in the working directory.

> **Security Note**: When using Basic Auth, it is highly recommended to run this server behind a reverse proxy (like Nginx/Caddy) with **HTTPS** enabled.

```toml
[server]
bind = "0.0.0.0"
port = 3032
db_path = "./data/tiddlers.sqlite3"
files_dir = "./files/"

# Display name for edits in the Wiki
[status]
username = "YourName" 

# [Optional] HTTP Basic Authentication
[auth]
username = "admin"
password = "change_me_please"

[s3]
enable = true
name = "r2"
access_key = "YOUR_AWS_ACCESS_KEY"
secret_key = "YOUR_AWS_SECRET_KEY"
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
region = "auto"
bucket_name = "your-wiki-assets"
public_url_base = "https://assets.your-domain.com"
```

## Quick Capture API (Inbox)

Capture thoughts from external tools without opening the wiki.

- **Endpoint**: `POST /api/inbox`
- **Content-Type**: `application/json`

### JSON Payload

```json
{
  "text": "This is a quick thought captured from my phone.",
  "tags": "idea mobile" 
}
```
*`tags` is optional. Default: "Inbox".*

### iOS / Android Shortcut Example
*   **URL**: `https://your-wiki.com/api/inbox`
*   **Method**: `POST`
*   **Headers**: `Authorization: Basic <Base64_Credentials>`
*   **Body**: JSON (Pass clipboard or input as `text`)

Captured items will appear in your Wiki with the tag `Inbox` and a timestamped title.

## Installation & Running

1.  **Build**:
    ```sh
    cargo build --release
    ```
2.  **Run**:
    Ensure `config.toml` and `empty.html` are in the directory.
    ```sh
    ./target/release/tiddly-wiki-server --config config.toml
    ```

## Development: Modifying the Embedded Plugin

This server embeds a custom TiddlyWiki plugin (`s3-uploader`) handling the drag-and-drop logic. If you want to modify the JavaScript or HTML of the uploader:

1.  Navigate to the `s3_uploader/` directory.
2.  Edit `s3-uploader.js` (logic) or `ui-modal.html` (UI) directly.
3.  **Repack the plugin** using the included tool:

    ```sh
    # Re-generates s3_uploader_plugin.json from source files
    cargo run --bin pack_plugin -- ./s3_uploader/manifest.json ./s3_uploader_plugin.json
    ```

4.  Rebuild the server (`cargo build`) to embed the changes.

## License

This project is made available under [The Prosperity Public License 3.0.0].

## Contributing

Pull requests are welcome. For major changes, please open an issue first to discuss what you would like to change.

## Code of Conduct

Contributors are expected to abide by the [Contributor Covenant](https://www.contributor-covenant.org/).