# 2026-11-02
- `WebViewEditor::with_close_handler()`: called with a `ParamSetter` when the host closes the editor (while the
  plugin is still alive), e.g. to end a parameter gesture a drag left open
- a web view that can't be created (Windows without the WebView2 Runtime) no longer panics in the host: the window
  says what to install on Windows, and `send_json()` / `resize()` do nothing without a web view

# 2024-09-10
- `WindowHandler::send_json()` doesn't return a `Result` anymore

# 2024-01-14
- update baseview and nih-plug
- switch from custom wry fork to the official version of wry since it now supports attaching to a raw window handle (thanks to [this fork by toiglak](https://github.com/toiglak/nih-plug-webview)!)
  - still need to verify how intercepting keyboard events works now
- drop Editor properly when window is closed (no more memory leaks hopefully)

# 2023-07-11
- start using baseview

# 2023-03-10
- macOS: add support for intercepting keys when the plugin UI is focused
