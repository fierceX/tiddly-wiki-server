import { defineConfig } from 'vite';
import path from 'path';

export default defineConfig({
  // 1. 设置基础路径为相对路径，这样你的 Rust 服务器挂载在 /foliate/ 下也能正常工作
  base: './', 
  
  build: {
    // 2. 指定输出目录
    outDir: 'ebook_reader',
    
    // 3. 明确告诉 Vite 入口文件是 reader.html
    rollupOptions: {
      input: {
        // 键名 'main' 可以随意，值必须是文件的绝对路径
        main: path.resolve(__dirname, 'reader.html') 
      }
    }
  }
});
