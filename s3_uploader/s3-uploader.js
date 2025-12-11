/*\
title: $:/plugins/custom/s3-uploader.js
type: application/javascript
module-type: startup

S3 Direct Uploader (With Native-like Import Report & Metadata Storage)
\*/
(function() {

/*jslint node: true, browser: true */
/*global $tw: false */
"use strict";

exports.name = "s3-uploader";
exports.platforms = ["browser"];
exports.after = ["startup"];
exports.synchronous = true;

// Global state
$tw.s3PendingFiles = [];
$tw.s3SuccessList = [];

exports.startup = function() {
    console.log("✅ S3 Uploader: Initializing...");
    if(typeof window !== 'undefined') {
        window.addEventListener("dragenter", onDragOver, true);
        window.addEventListener("dragover", onDragOver, true);
        window.addEventListener("drop", onDrop, true);
    }
    // 监听模态框确认按钮的消息
    $tw.rootWidget.addEventListener("tm-s3-confirm-upload", function(event) {
        $tw.s3SuccessList = [];
        processQueue();
    });
};

function onDragOver(event) { 
    event.preventDefault(); 
}

function onDrop(event) {
    var dataTransfer = event.dataTransfer;
    if (!dataTransfer || !dataTransfer.files || dataTransfer.files.length === 0) return;
    
    var file = dataTransfer.files[0];
    
    // 过滤掉普通的 TiddlyWiki 导入文件，交给核心处理
    if (file.name.endsWith(".tid") || file.name.endsWith(".json") || file.name.endsWith(".html")) return;
    
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation();
    
    $tw.s3PendingFiles = [file];
    
    var fileInfo = "<strong>File:</strong> " + file.name + "<br/><strong>Size:</strong> " + (file.size / 1024 / 1024).toFixed(2) + " MB";
    
    // 创建预览状态条目
    $tw.wiki.addTiddler(new $tw.Tiddler({title: "$:/state/s3-upload-preview", text: fileInfo}));
    
    // 打开确认模态框
    $tw.modal.display("$:/plugins/custom/s3-uploader/ui-modal");
}

function processQueue() {
    if ($tw.s3PendingFiles.length === 0) { 
        finishBatch(); 
        return; 
    }
    var file = $tw.s3PendingFiles.shift();
    uploadToS3(file);
}

function finishBatch() {
    if ($tw.s3SuccessList.length === 0) return;
    
    var listText = "The following files were successfully uploaded to S3 (Lazy Loaded):\n\n";
    $tw.s3SuccessList.forEach(function(title) { 
        listText += "* [[" + title + "]]\n"; 
    });
    
    var reportTitle = "$:/plugins/custom/s3-uploader/ui-report";
    
    // 创建导入报告条目
    $tw.wiki.addTiddler(new $tw.Tiddler({
        title: reportTitle, 
        text: listText, 
        tags: ["$:/tags/ImportResult"], 
        "caption": "S3 Upload Report", 
        "icon": "$:/core/images/cloud"
    }));
    
    $tw.notifier.display("✅ Batch upload complete");
    openTiddler(reportTitle);
}

function openTiddler(title) {
    var storyList = $tw.wiki.getTiddlerList("$:/StoryList");
    var index = storyList.indexOf(title);
    if (index !== -1) storyList.splice(index, 1);
    storyList.unshift(title);
    $tw.wiki.addTiddler(new $tw.Tiddler({title: "$:/StoryList", list: storyList}));
    setTimeout(function() {
        $tw.rootWidget.dispatchEvent({type: "tm-navigate", navigateTo: title, suppressNavigationHistory: false});
    }, 100);
}

function uploadToS3(file) {
    $tw.notifier.display("☁️ Requesting sign: " + file.name);
    
    // 1. 请求预签名 URL 和元数据
    fetch(`/api/sign-upload?filename=${encodeURIComponent(file.name)}&content_type=${encodeURIComponent(file.type)}`)
        .then(res => res.json())
        .then(data => {
            $tw.notifier.display("⬆️ Uploading...");
            
            // 2. 使用签名 URL 上传文件到 S3
            return fetch(data.upload_url, { 
                method: "PUT", 
                body: file, 
                headers: { "Content-Type": file.type } 
            }).then(res => { 
                if (res.ok) {
                    // 关键修改：返回完整的 data 对象，包含 key, bucket, region, public_url
                    return data; 
                }
                throw new Error("S3 Upload Failed"); 
            });
        })
        .then(data => {
            var title = file.name;
            
            // 3. 创建 Tiddler，并写入完整的 S3 元数据
            // 这样删除时，Rust 端可以直接读取 _s3_key 和 _s3_bucket 进行精准删除
            $tw.wiki.addTiddler(new $tw.Tiddler({
                title: title,
                type: file.type,
                text: "", // 保持为空，实现 Lazy Loading
                "_canonical_uri": data.public_url,
                
                // --- 新增元数据字段 ---
                "_file_storage": "s3",
                "_s3_key": data.key,
                "_s3_bucket": data.bucket,
                "_s3_region": data.region,
                "_s3_name": data.name
            }));
            
            $tw.s3SuccessList.push(title);
            $tw.rootWidget.dispatchEvent({type: "tm-auto-save-wiki"});
            
            // 继续处理下一个文件
            processQueue();
        })
        .catch(err => {
            console.error(err);
            $tw.notifier.display("❌ Error: " + err.message);
            // 即使出错也继续处理队列中的下一个
            processQueue();
        });
}

})();