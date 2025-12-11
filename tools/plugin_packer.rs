// 文件位置: src/bin/pack_plugin.rs
// 运行命令: cargo run --bin pack_plugin -- ./plugin_dev/manifest.json ./s3_uploader_plugin.json

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Deserialize, Debug)]
struct Manifest {
    // 插件本身的元数据
    title: String,
    name: Option<String>,
    description: Option<String>,
    author: Option<String>,
    version: Option<String>,
    #[serde(rename = "plugin-type")]
    plugin_type: Option<String>,
    
    // 包含的影子条目定义
    tiddlers: Vec<ShadowTiddlerConfig>,
}

#[derive(Deserialize, Debug)]
struct ShadowTiddlerConfig {
    title: String,
    file: String, // 相对路径，指向源码文件
    #[serde(flatten)]
    fields: HashMap<String, Value>, // 其他字段，如 module-type, tags 等
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: pack_plugin <manifest_path> <output_path>");
        std::process::exit(1);
    }

    let manifest_path = Path::new(&args[1]);
    let output_path = Path::new(&args[2]);
    let base_dir = manifest_path.parent().unwrap_or(Path::new("."));

    // 1. 读取清单文件
    let manifest_content = fs::read_to_string(manifest_path)?;
    let manifest: Manifest = serde_json::from_str(&manifest_content)?;

    println!("📦 Packing Plugin: {}", manifest.title);

    // 2. 构建 shadow tiddlers 的字典
    let mut shadow_tiddlers = HashMap::new();

    for item in &manifest.tiddlers {
        let file_path = base_dir.join(&item.file);
        println!("   ├── Reading: {} -> {}", item.file, item.title);
        
        let text_content = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;

        // 构建单个 shadow tiddler 的对象
        let mut tiddler_obj = json!({
            "text": text_content
        });

        // 合并 title 和其他字段
        let obj_map = tiddler_obj.as_object_mut().unwrap();
        // 显式插入 title
        // obj_map.insert("title".to_string(), Value::String(item.title.clone())); 
        // TiddlyWiki 插件内部 map 的 key 就是 title，通常内部对象不需要 title 字段，
        // 但为了保险起见，有些标准里也包含。标准做法是 key=title, value={text:..., type:...}

        // 合并 manifest 中定义的额外字段 (如 type, module-type)
        for (k, v) in &item.fields {
            obj_map.insert(k.clone(), v.clone());
        }

        shadow_tiddlers.insert(item.title.clone(), tiddler_obj);
    }

    // 3. 将 shadow tiddlers 序列化为字符串 (TiddlyWiki 插件的核心魔法)
    // 插件本身是一个 Tiddler，它的 'text' 字段是一个包含所有 shadow tiddlers 的 JSON 字符串
    let inner_json_str = serde_json::to_string(&json!({
        "tiddlers": shadow_tiddlers
    }))?;

    // 4. 构建最终的插件 Tiddler
    let mut plugin_final = json!({
        "title": manifest.title,
        "name": manifest.name.as_deref().unwrap_or("Custom Plugin"),
        "description": manifest.description.as_deref().unwrap_or(""),
        "author": manifest.author.as_deref().unwrap_or("RustPacker"),
        "version": manifest.version.as_deref().unwrap_or("0.0.1"),
        "plugin-type": manifest.plugin_type.as_deref().unwrap_or("plugin"),
        "type": "application/json", // 插件本身的类型
        "text": inner_json_str      // 核心内容
    });

    // 5. 输出为数组格式 (TiddlyWiki 导入标准通常是数组)
    let output_json = serde_json::to_string_pretty(&json!([plugin_final]))?;
    
    fs::write(output_path, output_json)?;

    println!("✅ Done! Plugin saved to: {}", output_path.display());
    Ok(())
}