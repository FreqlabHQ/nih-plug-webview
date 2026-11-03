use baseview::{Event, Size, Window, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use nih_plug::prelude::{Editor, GuiContext, ParamSetter};
use serde_json::Value;
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    http::{Request, Response},
    Rect, WebContext, WebView, WebViewBuilder,
};

use crossbeam::channel::{unbounded, Receiver};

// raw-window-handle 0.5 (what baseview provides)
use raw_window_handle_05 as rwh05;
// raw-window-handle 0.6 (what wry expects)
use raw_window_handle as rwh06;

pub use wry::http;

pub use baseview::{DropData, DropEffect, EventStatus, MouseEvent};
pub use keyboard_types::*;

/// Bridge between raw-window-handle 0.5 (baseview) and 0.6 (wry)
struct Rwh06Wrapper<'a> {
    window: &'a baseview::Window<'a>,
}

impl<'a> rwh06::HasWindowHandle for Rwh06Wrapper<'a> {
    fn window_handle(&self) -> Result<rwh06::WindowHandle<'_>, rwh06::HandleError> {
        use rwh05::HasRawWindowHandle;
        let raw = self.window.raw_window_handle();

        // Convert rwh 0.5 handle to rwh 0.6 handle
        let handle = match raw {
            #[cfg(target_os = "macos")]
            rwh05::RawWindowHandle::AppKit(h) => {
                let new_handle = rwh06::AppKitWindowHandle::new(
                    std::ptr::NonNull::new(h.ns_view as *mut _).expect("NSView pointer is null")
                );
                rwh06::RawWindowHandle::AppKit(new_handle)
            }
            #[cfg(target_os = "windows")]
            rwh05::RawWindowHandle::Win32(h) => {
                let mut new_handle = rwh06::Win32WindowHandle::new(
                    std::num::NonZeroIsize::new(h.hwnd as isize).expect("HWND is null")
                );
                new_handle.hinstance = std::num::NonZeroIsize::new(h.hinstance as isize);
                rwh06::RawWindowHandle::Win32(new_handle)
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            rwh05::RawWindowHandle::Xlib(h) => {
                let new_handle = rwh06::XlibWindowHandle::new(h.window);
                rwh06::RawWindowHandle::Xlib(new_handle)
            }
            _ => panic!("Unsupported platform"),
        };

        Ok(unsafe { rwh06::WindowHandle::borrow_raw(handle) })
    }
}

impl<'a> rwh06::HasDisplayHandle for Rwh06Wrapper<'a> {
    fn display_handle(&self) -> Result<rwh06::DisplayHandle<'_>, rwh06::HandleError> {
        use rwh05::HasRawDisplayHandle;
        let raw = self.window.raw_display_handle();

        let handle = match raw {
            #[cfg(target_os = "macos")]
            rwh05::RawDisplayHandle::AppKit(_) => rwh06::RawDisplayHandle::AppKit(rwh06::AppKitDisplayHandle::new()),
            #[cfg(target_os = "windows")]
            rwh05::RawDisplayHandle::Windows(_) => rwh06::RawDisplayHandle::Windows(rwh06::WindowsDisplayHandle::new()),
            #[cfg(all(unix, not(target_os = "macos")))]
            rwh05::RawDisplayHandle::Xlib(h) => {
                let new_handle = rwh06::XlibDisplayHandle::new(
                    std::ptr::NonNull::new(h.display),
                    h.screen
                );
                rwh06::RawDisplayHandle::Xlib(new_handle)
            }
            _ => panic!("Unsupported platform"),
        };

        Ok(unsafe { rwh06::DisplayHandle::borrow_raw(handle) })
    }
}

type EventLoopHandler = dyn Fn(&WindowHandler, ParamSetter, &mut Window) + Send + Sync;
type KeyboardHandler = dyn Fn(KeyboardEvent) -> bool + Send + Sync;
type MouseHandler = dyn Fn(MouseEvent) -> EventStatus + Send + Sync;
type CustomProtocolHandler =
    dyn Fn(&Request<Vec<u8>>) -> wry::Result<Response<Cow<'static, [u8]>>> + Send + Sync;
type CloseHandler = dyn Fn(ParamSetter) + Send + Sync;

/// Shown in the editor instead of the UI when the web view can't be created (Windows: no WebView2)
#[cfg(target_os = "windows")]
const WEBVIEW_MISSING: &str = "This plugin's window needs the Microsoft Edge WebView2 Runtime, a free download \
from Microsoft. Install it, then reopen this window. The plugin's sound keeps working.";

pub struct WebViewEditor {
    source: Arc<HTMLSource>,
    width: Arc<AtomicU32>,
    height: Arc<AtomicU32>,
    event_loop_handler: Arc<EventLoopHandler>,
    keyboard_handler: Arc<KeyboardHandler>,
    mouse_handler: Arc<MouseHandler>,
    close_handler: Arc<CloseHandler>,
    custom_protocol: Option<(String, Arc<CustomProtocolHandler>)>,
    developer_mode: bool,
    background_color: (u8, u8, u8, u8),
}

pub enum HTMLSource {
    String(&'static str),
    URL(&'static str),
}

impl WebViewEditor {
    pub fn new(source: HTMLSource, size: (u32, u32)) -> Self {
        let width = Arc::new(AtomicU32::new(size.0));
        let height = Arc::new(AtomicU32::new(size.1));
        Self {
            source: Arc::new(source),
            width,
            height,
            developer_mode: false,
            background_color: (255, 255, 255, 255),
            event_loop_handler: Arc::new(|_, _, _| {}),
            keyboard_handler: Arc::new(|_| false),
            mouse_handler: Arc::new(|_| EventStatus::Ignored),
            close_handler: Arc::new(|_| {}),
            custom_protocol: None,
        }
    }

    pub fn with_background_color(mut self, background_color: (u8, u8, u8, u8)) -> Self {
        self.background_color = background_color;
        self
    }

    pub fn with_custom_protocol<F>(mut self, name: String, handler: F) -> Self
    where
        F: Fn(&Request<Vec<u8>>) -> wry::Result<Response<Cow<'static, [u8]>>>
            + 'static
            + Send
            + Sync,
    {
        self.custom_protocol = Some((name, Arc::new(handler)));
        self
    }

    pub fn with_event_loop<F>(mut self, handler: F) -> Self
    where
        F: Fn(&WindowHandler, ParamSetter, &mut baseview::Window) + 'static + Send + Sync,
    {
        self.event_loop_handler = Arc::new(handler);
        self
    }

    pub fn with_developer_mode(mut self, mode: bool) -> Self {
        self.developer_mode = mode;
        self
    }

    pub fn with_keyboard_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(KeyboardEvent) -> bool + Send + Sync + 'static,
    {
        self.keyboard_handler = Arc::new(handler);
        self
    }

    pub fn with_mouse_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(MouseEvent) -> EventStatus + Send + Sync + 'static,
    {
        self.mouse_handler = Arc::new(handler);
        self
    }

    /// Called when the editor window closes, e.g. to end a parameter gesture a drag left open
    /// (the page's own "drag ended" message never arrives once the window is gone)
    pub fn with_close_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(ParamSetter) + Send + Sync + 'static,
    {
        self.close_handler = Arc::new(handler);
        self
    }
}

pub struct WindowHandler {
    context: Arc<dyn GuiContext>,
    event_loop_handler: Arc<EventLoopHandler>,
    keyboard_handler: Arc<KeyboardHandler>,
    mouse_handler: Arc<MouseHandler>,
    /// None when it couldn't be created (the window then shows `WEBVIEW_MISSING` on Windows)
    webview: Option<WebView>,
    events_receiver: Receiver<Value>,
    pub width: Arc<AtomicU32>,
    pub height: Arc<AtomicU32>,
}

impl WindowHandler {
    pub fn resize(&self, window: &mut baseview::Window, width: u32, height: u32) {
        if let Some(webview) = &self.webview {
            let _ = webview.set_bounds(Rect {
                position: LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(width, height).into(),
            });
        }
        self.width.store(width, Ordering::Relaxed);
        self.height.store(height, Ordering::Relaxed);
        self.context.request_resize();
        window.resize(Size {
            width: width as f64,
            height: height as f64,
        });
    }

    pub fn send_json(&self, json: Value) {
        let Some(webview) = &self.webview else { return };
        let json_str = json.to_string();
        let json_str_quoted =
            serde_json::to_string(&json_str).expect("Should not fail: the value is always string");
        // (a failure here must not take the host down with it)
        if let Err(e) = webview
            .evaluate_script(&format!("onPluginMessageInternal({});", json_str_quoted))
        {
            nih_plug::nih_log!("Couldn't send a message to the web view: {}", e);
        }
    }

    pub fn next_event(&self) -> Result<Value, crossbeam::channel::TryRecvError> {
        self.events_receiver.try_recv()
    }
}

/// Windows: a plain text child window saying why there's no UI
#[cfg(target_os = "windows")]
fn show_webview_missing(window: &baseview::Window, width: u32, height: u32) {
    use rwh05::HasRawWindowHandle;
    #[repr(C)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetClientRect(window: isize, rect: *mut RECT) -> i32;
        fn SetWindowPos(window: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn SendMessageW(window: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn CreateWindowExW(
            ex_style: u32,
            class_name: *const u16,
            window_name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: isize,
            menu: isize,
            instance: isize,
            param: *mut std::ffi::c_void,
        ) -> isize;
    }
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    #[link(name = "gdi32")]
    extern "system" {
        fn GetStockObject(object: i32) -> isize;
    }
    const SS_CENTER: u32 = 0x0000_0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOSIZE: u32 = 0x0001;
    const WM_SETFONT: u32 = 0x0030;
    const DEFAULT_GUI_FONT: i32 = 17;
    let rwh05::RawWindowHandle::Win32(handle) = window.raw_window_handle() else { return };
    let parent = handle.hwnd as isize;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (class_name, text) = (wide("STATIC"), wide(WEBVIEW_MISSING));
    // (a child of the editor window: it goes when the window does)
    unsafe {
        // The client area in physical pixels (the logical size is smaller on a scaled display)
        let mut rect = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        GetClientRect(parent, &mut rect);
        let label = CreateWindowExW(
            0,
            class_name.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_CENTER,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            parent,
            0,
            handle.hinstance as isize,
            std::ptr::null_mut(),
        );
        if label != 0 {
            // (above the empty container wry may have left behind)
            SetWindowPos(label, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            SendMessageW(label, WM_SETFONT, GetStockObject(DEFAULT_GUI_FONT) as usize, 1);
        }
    }
}

impl baseview::WindowHandler for WindowHandler {
    fn on_frame(&mut self, window: &mut baseview::Window) {
        let setter = ParamSetter::new(&*self.context);
        (self.event_loop_handler)(&self, setter, window);
    }

    fn on_event(&mut self, _window: &mut baseview::Window, event: Event) -> EventStatus {
        match event {
            Event::Keyboard(event) => {
                if (self.keyboard_handler)(event) {
                    EventStatus::Captured
                } else {
                    EventStatus::Ignored
                }
            }
            Event::Mouse(mouse_event) => (self.mouse_handler)(mouse_event),
            _ => EventStatus::Ignored,
        }
    }
}

struct Instance {
    window_handle: WindowHandle,
    context: Arc<dyn GuiContext>,
    close_handler: Arc<CloseHandler>,
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Here, while the host is closing the editor (the plugin is still alive), rather than when
        // the window goes, which on Windows is later, when the host next handles its messages
        (self.close_handler)(ParamSetter::new(&*self.context));
        self.window_handle.close();
    }
}

unsafe impl Send for Instance {}

impl Editor for WebViewEditor {
    fn spawn(
        &self,
        parent: nih_plug::prelude::ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        let options = WindowOpenOptions {
            scale: WindowScalePolicy::SystemScaleFactor,
            size: Size {
                width: self.width.load(Ordering::Relaxed) as f64,
                height: self.height.load(Ordering::Relaxed) as f64,
            },
            title: "Plug-in".to_owned(),
        };

        let width = self.width.clone();
        let height = self.height.clone();
        let developer_mode = self.developer_mode;
        let source = self.source.clone();
        let background_color = self.background_color;
        let custom_protocol = self.custom_protocol.clone();
        let event_loop_handler = self.event_loop_handler.clone();
        let keyboard_handler = self.keyboard_handler.clone();
        let mouse_handler = self.mouse_handler.clone();
        let close_handler = self.close_handler.clone();
        let instance_context = context.clone();

        let window_handle = baseview::Window::open_parented(&parent, options, move |window| {
            let (events_sender, events_receiver) = unbounded();

            let mut web_context = WebContext::new(Some(std::env::temp_dir()));

            let w = width.load(Ordering::Relaxed);
            let h = height.load(Ordering::Relaxed);
            let mut webview_builder = WebViewBuilder::new_with_web_context(&mut web_context)
                .with_bounds(Rect {
                    position: LogicalPosition::new(0, 0).into(),
                    size: LogicalSize::new(w, h).into(),
                })
                .with_accept_first_mouse(true)
                .with_devtools(developer_mode)
                .with_initialization_script(include_str!("script.js"))
                .with_ipc_handler(move |request| {
                    let msg = request.body();
                    match serde_json::from_str(msg) {
                        Ok(json_value) => {
                            let _ = events_sender.send(json_value);
                        }
                        // (the page's mistake: ignored, never a crash in the host)
                        Err(e) => nih_plug::nih_log!("Ignored a message from the web view that isn't JSON ({}): {}", e, msg),
                    }
                })
                .with_background_color(background_color);

            if let Some(custom_protocol) = custom_protocol.as_ref() {
                let handler = custom_protocol.1.clone();
                webview_builder = webview_builder
                    .with_asynchronous_custom_protocol(custom_protocol.0.to_owned(), move |_webview_id, request, responder| {
                        match handler(&request) {
                            Ok(response) => responder.respond(response),
                            Err(e) => {
                                nih_plug::nih_log!("Custom protocol request failed: {}", e);
                                let mut response = Response::new(Cow::Borrowed(&[][..]));
                                *response.status_mut() = http::StatusCode::INTERNAL_SERVER_ERROR;
                                responder.respond(response);
                            }
                        }
                    });
            }

            let webview_builder = match source.as_ref() {
                HTMLSource::String(html_str) => webview_builder.with_html(*html_str),
                HTMLSource::URL(url) => webview_builder.with_url(*url),
            };
            // Wrap window to bridge rwh 0.5 -> 0.6
            let window_wrapper = Rwh06Wrapper { window };
            // (a failure, such as Windows without WebView2, must not take the host down with it)
            let webview = match webview_builder.build_as_child(&window_wrapper) {
                Ok(webview) => Some(webview),
                Err(e) => {
                    nih_plug::nih_log!("Couldn't create the web view: {}", e);
                    #[cfg(target_os = "windows")]
                    show_webview_missing(window, w, h);
                    None
                }
            };

            WindowHandler {
                context,
                event_loop_handler,
                webview,
                events_receiver,
                keyboard_handler,
                mouse_handler,
                width,
                height,
            }
        });
        return Box::new(Instance {
            window_handle,
            context: instance_context,
            close_handler,
        });
    }

    fn size(&self) -> (u32, u32) {
        (
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
        )
    }

    fn set_scale_factor(&self, _factor: f32) -> bool {
        // TODO: implement for Windows and Linux
        return false;
    }

    fn param_values_changed(&self) {}

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}
}
