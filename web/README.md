# Custom Foliate Build

This is a customized build of foliate-js for embedding in TiddlyWiki/Rust.

## Modifications
1. **Removed PDF**: Commented out PDF and ComicBook imports in `view.js` to reduce bundle size.
2. **Build System**: Added `vite.config.js` for bundling `reader.html`.
3. **Dependencies**: Fixed `rollup/zip.js` to point to `@zip.js/zip.js`.
4. **Modify the entrance page.**: Modify the `reader.html` file to allow it to open a specified file by accepting parameters.

## How to Build
1. `npm install`
2. `npx vite build`
3. The artifacts will be in `./ebook_reader` (Rust embeds this folder).