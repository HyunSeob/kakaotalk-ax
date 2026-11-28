//! Platform dispatch creating the native live backend for macOS or Windows.

use crate::ax_core::KakaoBackend;

#[cfg(target_os = "macos")]
pub fn create_live_backend() -> Box<dyn KakaoBackend> {
    use crate::ax_core::AxEngine;
    use crate::macos_live::MacosAxDriver;
    Box::new(AxEngine::new(MacosAxDriver::new()))
}

#[cfg(target_os = "windows")]
pub fn create_live_backend() -> Box<dyn KakaoBackend> {
    use std::sync::Arc;
    use crate::win32_clipboard::Clipboard;
    use crate::win32_engine::WindowsAx;
    use crate::win32_live::{LiveClipboardSys, Win32Live};
    use crate::win32_ocr::WinRtOcr;

    let api = Arc::new(Win32Live::new());
    let clipboard = Arc::new(Clipboard::new(Arc::clone(&api), LiveClipboardSys));
    let ocr = Arc::new(WinRtOcr::new());
    Box::new(WindowsAx::new(api, clipboard, ocr))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn create_live_backend() -> Box<dyn KakaoBackend> {
    use serde_json::Value;
    use crate::error::{KaxError, Result};

    struct UnsupportedBackend;
    impl KakaoBackend for UnsupportedBackend {
        fn build(&self, _kind: &str) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn windows(&self) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn rooms(&self, _query: &str, _scroll: bool, _limit: i64) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn search_rooms(&self, _query: &str) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn open_room(&self, _window_title: &str, _require_self: bool) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn close_room(&self, _window_title: &str) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn members(&self, _window_title: &str, _limit: i64) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn read_messages(&self, _window_title: &str, _limit: i64, _offset: i64, _snapshot_id: &str) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn read_attachment(&self, _window_title: &str, _row_index: i64, _snapshot_id: &str, _output_path: Option<&str>, _ocr: bool) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn read_many(&self, _window_titles: &[String], _limit: i64, _open_if_needed: bool, _close_after: bool, _max_workers: i64) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn load_more(&self, _window_title: &str, _pages: i64) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn search_messages(&self, _window_title: &str, _query: &str, _limit: i64, _load_pages: i64, _case_sensitive: bool, _use_app_search: bool, _max_steps: i64, _context_rows: i64) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn close_search(&self, _window_title: &str) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn send_message(&self, _window_title: &str, _text: &str, _operation_id: &str, _wait_seconds: f64, _require_self: bool) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
        fn send_result(&self, _operation_id: &str, _wait_seconds: f64) -> Result<Value> {
            Err(KaxError::validation("macOS or Windows is required"))
        }
    }
    Box::new(UnsupportedBackend)
}
