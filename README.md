# nih-plug-webview (freqlab fork)

Fork of [nih-plug-webview](https://github.com/httnn/nih-plug-webview) with patches for **Tauri app compatibility** and **hot reload support**.

## Why this fork?

When loading webview-based CLAP/VST3 plugins inside a Tauri application (like [freqlab](https://github.com/jamesontucker/freqlab)), both the host app and the plugin use [wry](https://github.com/tauri-apps/wry) for WebView rendering. This causes Objective-C class name conflicts on macOS because both try to register the same class names (e.g., `WryWebView`).

This fork solves that by:

1. **Prefixed class names** - All wry Objective-C classes use `NihPlugWry*` prefix (e.g., `NihPlugWryWebView_12345678`)
2. **Dynamic class suffix** - Each build gets a unique suffix via `WRY_BUILD_SUFFIX` env var, enabling hot reload
3. **Plugin host fixes** - Graceful handling when NSView isn't attached to a window yet
4. **Updated to wry 0.53.5** - Matches Tauri 2.x's wry version

## Hot Reload Support

For hot reload to work, set `WRY_BUILD_SUFFIX` to a unique value before each build:

```rust
// In your build script or xtask
let timestamp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis();
std::env::set_var("WRY_BUILD_SUFFIX", format!("{}", timestamp % 100_000_000));
```

This ensures each plugin build registers unique class names, preventing conflicts during hot reload.

## Usage

```toml
[dependencies]
nih_plug_webview = { git = "https://github.com/jamesontucker/nih-plug-webview" }
```

## Features

- Send arbitrary JSON values to/from the webview using Serde
- Resizable plugin window
- Drag and drop files with full paths
- Callback for filtering key events from DAW
- Customizable background color
- DevTools support

## Original

Based on [httnn/nih-plug-webview](https://github.com/httnn/nih-plug-webview) by Max Huttunen.
