// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use once_cell::sync::Lazy;
use windows::{
  core::{HRESULT, HSTRING, PCSTR},
  Win32::{
    Foundation::{FARPROC, HWND, S_OK},
    Graphics::Gdi::{
      GetDC, GetDeviceCaps, MonitorFromWindow, HMONITOR, LOGPIXELSX, MONITOR_DEFAULTTONEAREST,
    },
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    UI::{
      HiDpi::{MDT_EFFECTIVE_DPI, MONITOR_DPI_TYPE},
      WindowsAndMessaging::IsProcessDPIAware,
    },
  },
};

fn get_function_impl(library: &str, function: &str) -> FARPROC {
  let library = HSTRING::from(library);
  assert_eq!(function.chars().last(), Some('\0'));

  // Library names we will use are ASCII so we can use the A version to avoid string conversion.
  let module = unsafe { LoadLibraryW(&library) }.unwrap_or_default();
  if module.is_invalid() {
    return None;
  }

  unsafe { GetProcAddress(module, PCSTR::from_raw(function.as_ptr())) }
}

macro_rules! get_function {
  ($lib:expr, $func:ident) => {
    crate::webview2::util::get_function_impl($lib, concat!(stringify!($func), '\0'))
      .map(|f| unsafe { std::mem::transmute::<_, $func>(f) })
  };
}

pub type GetDpiForWindow = unsafe extern "system" fn(hwnd: HWND) -> u32;
pub type GetDpiForMonitor = unsafe extern "system" fn(
  hmonitor: HMONITOR,
  dpi_type: MONITOR_DPI_TYPE,
  dpi_x: *mut u32,
  dpi_y: *mut u32,
) -> HRESULT;

static GET_DPI_FOR_WINDOW: Lazy<Option<GetDpiForWindow>> =
  Lazy::new(|| get_function!("user32.dll", GetDpiForWindow));
static GET_DPI_FOR_MONITOR: Lazy<Option<GetDpiForMonitor>> =
  Lazy::new(|| get_function!("shcore.dll", GetDpiForMonitor));

pub const BASE_DPI: u32 = 96;
pub fn dpi_to_scale_factor(dpi: u32) -> f64 {
  dpi as f64 / BASE_DPI as f64
}

#[allow(non_snake_case)]
pub unsafe fn hwnd_dpi(hwnd: HWND) -> u32 {
  if let Some(GetDpiForWindow) = *GET_DPI_FOR_WINDOW {
    // We are on Windows 10 Anniversary Update (1607) or later.
    match GetDpiForWindow(hwnd) {
      0 => BASE_DPI, // 0 is returned if hwnd is invalid
      #[allow(clippy::unnecessary_cast)]
      dpi => dpi as u32,
    }
  } else if let Some(GetDpiForMonitor) = *GET_DPI_FOR_MONITOR {
    // We are on Windows 8.1 or later.
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if monitor.is_invalid() {
      return BASE_DPI;
    }

    let mut dpi_x = 0;
    let mut dpi_y = 0;
    #[allow(clippy::unnecessary_cast)]
    if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == S_OK {
      dpi_x as u32
    } else {
      BASE_DPI
    }
  } else {
    let hdc = GetDC(Some(hwnd));
    if hdc.is_invalid() {
      return BASE_DPI;
    }

    // We are on Vista or later.
    if IsProcessDPIAware().as_bool() {
      // If the process is DPI aware, then scaling must be handled by the application using
      // this DPI value.
      GetDeviceCaps(Some(hdc), LOGPIXELSX) as u32
    } else {
      // If the process is DPI unaware, then scaling is performed by the OS; we thus return
      // 96 (scale factor 1.0) to prevent the window from being re-scaled by both the
      // application and the WM.
      BASE_DPI
    }
  }
}

/// How long a WebView2 call may take to answer before it's treated as failed.
pub const CREATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Pumps this thread's messages until `rx` has a result, like `webview2_com::wait_with_pump`, but stops when the
/// sender is gone (a callback that ended without sending) or after `timeout`, rather than waiting forever: in a
/// plugin, a wait that never ends freezes the host.
pub fn wait_with_pump<T>(
  rx: std::sync::mpsc::Receiver<T>,
  timeout: std::time::Duration,
) -> windows::core::Result<T> {
  use std::sync::mpsc::TryRecvError;
  use windows::Win32::{
    Foundation::{E_ABORT, ERROR_TIMEOUT},
    UI::WindowsAndMessaging::{
      DispatchMessageA, MsgWaitForMultipleObjects, PeekMessageA, PostQuitMessage,
      TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
    },
  };

  let deadline = std::time::Instant::now() + timeout;
  let mut msg = MSG::default();
  loop {
    match rx.try_recv() {
      Ok(result) => return Ok(result),
      Err(TryRecvError::Disconnected) => return Err(E_ABORT.into()),
      Err(TryRecvError::Empty) => {}
    }
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
      return Err(ERROR_TIMEOUT.to_hresult().into());
    }
    unsafe {
      MsgWaitForMultipleObjects(
        None,
        false,
        left.as_millis().min(u32::MAX as u128) as u32,
        QS_ALLINPUT,
      );
      while PeekMessageA(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
        if msg.message == WM_QUIT {
          // (the host's own quit: put back for its loop to see)
          PostQuitMessage(msg.wParam.0 as i32);
          return Err(E_ABORT.into());
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageA(&msg);
      }
    }
  }
}
