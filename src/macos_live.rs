//! Live macOS Accessibility (`ApplicationServices`), `ScreenCaptureKit`, `Vision` OCR,
//! and `ImageIO` PNG export implemented directly in Rust.
//!
//! Matches `ReadKakaoAX.swift`, `ControlKakaoAX.swift`, and `SendKakao.swift`
//! for both Apple Silicon (`aarch64-apple-darwin`) and Intel (`x86_64-apple-darwin`).

#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use block2::RcBlock;
use serde_json::{json, Map, Value};

use crate::ax_core::AxDriver;
use crate::error::{KaxError, Result};
use crate::macos_rows::{centered_layout, own_side_layout, row_shape, LayoutNeed, SenderContext};
use crate::macos_ax_logic::{
    call_deadline, call_timeout, chat_window_check, check_search_toolbar_geometry,
    close_allowed_composers, is_macos_self_alias, member_item, member_limit_stop,
    parse_member_button, read_timeout, readable_transcript_check, resolve_self_menu_outcome,
    match_room_name, parse_folder_label, parse_room_row_labels, room_scroll_step, room_scroll_stop,
    ROOM_FOLDER_LIST_TITLE,
    room_row_is_self, split_room_type_prefix, verify_bound_self_window, ROOM_BADGE_SELF,
    AxFrame, COMPOSER_LABELS, MAX_MEMBER_LIMIT, MAX_ROOM_SCROLL_LIMIT, READ_ROW_CAP, SEARCH_ROW_CAP, SEND_TAIL_MAX_ROWS, SEARCH_TOOLBAR_BUTTON_COUNT,
    SELF_MENU_TITLES, SELF_PROFILE_LABELS, SEND_BUTTON_TITLES,
};

// ---------------------------------------------------------------------------
// CoreFoundation / ApplicationServices / CoreGraphics / ImageIO / ObjC FFI
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFNumberRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFDataRef = *const c_void;
type CFMutableDataRef = *mut c_void;
type AXUIElementRef = *const c_void;
type AXValueRef = *const c_void;
type CGImageRef = *const c_void;
type CFTypeID = usize;
type CFIndex = isize;
type CFHashCode = usize;
type AXError = i32;
type AXValueType = u32;

const K_AX_ERROR_SUCCESS: AXError = 0;
const K_AX_ERROR_FAILURE: AXError = -25200;
const K_AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
const K_AX_ERROR_NOT_IMPLEMENTED: AXError = -25208;
const K_AX_ERROR_NO_VALUE: AXError = -25212;

const K_AX_VALUE_CGPOINT_TYPE: AXValueType = 1;
const K_AX_VALUE_CGSIZE_TYPE: AXValueType = 2;
const K_AX_VALUE_CGRECT_TYPE: AXValueType = 3;
const K_AX_VALUE_CFRANGE_TYPE: AXValueType = 4;
const K_AX_VALUE_AXERROR_TYPE: AXValueType = 5;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_NUMBER_SINT64_TYPE: i32 = 4;
const K_CF_NUMBER_FLOAT64_TYPE: i32 = 6;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

impl CGRect {
    fn min_x(&self) -> f64 {
        self.origin.x
    }
    fn min_y(&self) -> f64 {
        self.origin.y
    }
    fn max_x(&self) -> f64 {
        self.origin.x + self.size.width
    }
    fn max_y(&self) -> f64 {
        self.origin.y + self.size.height
    }
    fn mid_y(&self) -> f64 {
        self.origin.y + self.size.height * 0.5
    }
    fn inset_by(&self, dx: f64, dy: f64) -> Self {
        Self {
            origin: CGPoint {
                x: self.origin.x + dx,
                y: self.origin.y + dy,
            },
            size: CGSize {
                width: (self.size.width - 2.0 * dx).max(0.0),
                height: (self.size.height - 2.0 * dy).max(0.0),
            },
        }
    }
    fn contains(&self, other: &CGRect) -> bool {
        other.min_x() >= self.min_x()
            && other.min_y() >= self.min_y()
            && other.max_x() <= self.max_x()
            && other.max_y() <= self.max_y()
    }
    fn integral(&self) -> Self {
        let x1 = self.min_x().floor();
        let y1 = self.min_y().floor();
        let x2 = self.max_x().ceil();
        let y2 = self.max_y().ceil();
        Self {
            origin: CGPoint { x: x1, y: y1 },
            size: CGSize {
                width: (x2 - x1).max(0.0),
                height: (y2 - y1).max(0.0),
            },
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CFRange {
    location: CFIndex,
    length: CFIndex,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFBooleanRef;
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFEqual(cf1: CFTypeRef, cf2: CFTypeRef) -> u8;
    fn CFHash(cf: CFTypeRef) -> CFHashCode;
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFNullGetTypeID() -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFAttributedStringGetTypeID() -> CFTypeID;
    fn CFAttributedStringGetString(astr: CFTypeRef) -> CFStringRef;
    fn CFArrayGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFBooleanGetValue(b: CFBooleanRef) -> u8;
    fn CFNumberCreate(
        allocator: CFTypeRef,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
    fn CFNumberGetValue(number: CFNumberRef, the_type: i32, value_ptr: *mut c_void) -> u8;
    fn CFStringCreateWithBytes(
        alloc: CFTypeRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external: u8,
    ) -> CFStringRef;
    fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: u32,
    ) -> u8;
    fn CFArrayCreate(
        allocator: CFTypeRef,
        values: *const CFTypeRef,
        num_values: CFIndex,
        callbacks: *const c_void,
    ) -> CFArrayRef;
    fn CFArrayGetCount(the_array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: CFIndex) -> CFTypeRef;
    fn CFDataCreateMutable(allocator: CFTypeRef, capacity: CFIndex) -> CFMutableDataRef;
    fn CFDataGetLength(the_data: CFDataRef) -> CFIndex;
    fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_in_seconds: f32) -> AXError;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyMultipleAttributeValues(
        element: AXUIElementRef,
        attributes: CFArrayRef,
        options: u32,
        values: *mut CFArrayRef,
    ) -> AXError;
    fn AXUIElementGetAttributeValueCount(
        element: AXUIElementRef,
        attribute: CFStringRef,
        count: *mut CFIndex,
    ) -> AXError;
    fn AXUIElementCopyAttributeValues(
        element: AXUIElementRef,
        attribute: CFStringRef,
        index: CFIndex,
        max_values: CFIndex,
        values: *mut CFArrayRef,
    ) -> AXError;
    fn AXUIElementCopyActionNames(element: AXUIElementRef, names: *mut CFArrayRef) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXValueGetType(value: AXValueRef) -> AXValueType;
    fn AXValueGetValue(value: AXValueRef, the_type: AXValueType, value_ptr: *mut c_void) -> u8;
    fn AXValueCreate(the_type: AXValueType, value_ptr: *const c_void) -> AXValueRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> u8;
    fn CGImageRetain(image: CGImageRef) -> CGImageRef;
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGImageCreateWithImageInRect(image: CGImageRef, rect: CGRect) -> CGImageRef;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithData(
        data: CFMutableDataRef,
        uti: CFStringRef,
        count: usize,
        options: CFTypeRef,
    ) -> *mut c_void;
    fn CGImageDestinationAddImage(dest: *mut c_void, image: CGImageRef, properties: CFTypeRef);
    fn CGImageDestinationFinalize(dest: *mut c_void) -> u8;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "Vision", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
}

// ---------------------------------------------------------------------------
// RAII CoreFoundation wrappers
// ---------------------------------------------------------------------------

struct CfRef(CFTypeRef);

unsafe impl Send for CfRef {}

impl CfRef {
    unsafe fn from_owned(ptr: CFTypeRef) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr))
        }
    }

    unsafe fn from_borrowed(ptr: CFTypeRef) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            CFRetain(ptr);
            Some(Self(ptr))
        }
    }

    fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Clone for CfRef {
    fn clone(&self) -> Self {
        unsafe {
            CFRetain(self.0);
        }
        Self(self.0)
    }
}

impl Drop for CfRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(s: &str) -> CfRef {
    let ptr = unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            K_CF_STRING_ENCODING_UTF8,
            0,
        )
    };
    CfRef(ptr)
}

fn cf_to_string(ptr: CFTypeRef) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let tid = unsafe { CFGetTypeID(ptr) };
    let str_ptr = if tid == unsafe { CFStringGetTypeID() } {
        ptr
    } else if tid == unsafe { CFAttributedStringGetTypeID() } {
        unsafe { CFAttributedStringGetString(ptr) }
    } else {
        return None;
    };
    if str_ptr.is_null() {
        return Some(String::new());
    }
    let len = unsafe { CFStringGetLength(str_ptr) };
    if len == 0 {
        return Some(String::new());
    }
    let max_size = unsafe { CFStringGetMaximumSizeForEncoding(len, K_CF_STRING_ENCODING_UTF8) } + 1;
    let mut buf = vec![0u8; max_size as usize];
    let ok = unsafe {
        CFStringGetCString(
            str_ptr,
            buf.as_mut_ptr() as *mut c_char,
            max_size,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    Some(cstr.to_string_lossy().into_owned())
}

fn cf_to_bool(ptr: CFTypeRef) -> Option<bool> {
    if ptr.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(ptr) == CFBooleanGetTypeID() } {
        return Some(unsafe { CFBooleanGetValue(ptr) != 0 });
    }
    None
}

fn cf_to_f64(ptr: CFTypeRef) -> Option<f64> {
    if ptr.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(ptr) == CFNumberGetTypeID() } {
        let mut v: f64 = 0.0;
        if unsafe { CFNumberGetValue(ptr, K_CF_NUMBER_FLOAT64_TYPE, (&mut v as *mut f64).cast()) != 0 }
        {
            return Some(v);
        }
    }
    None
}

fn cf_to_i64(ptr: CFTypeRef) -> Option<i64> {
    if ptr.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(ptr) == CFNumberGetTypeID() } {
        let mut v: i64 = 0;
        if unsafe { CFNumberGetValue(ptr, K_CF_NUMBER_SINT64_TYPE, (&mut v as *mut i64).cast()) != 0 }
        {
            return Some(v);
        }
    }
    None
}

fn cf_number_f64(v: f64) -> CfRef {
    let ptr = unsafe {
        CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_FLOAT64_TYPE,
            (&v as *const f64).cast(),
        )
    };
    CfRef(ptr)
}

fn cf_to_elements(ptr: CFTypeRef) -> Vec<CfRef> {
    if ptr.is_null() || unsafe { CFGetTypeID(ptr) != CFArrayGetTypeID() } {
        return Vec::new();
    }
    let count = unsafe { CFArrayGetCount(ptr) };
    let mut out = Vec::with_capacity(count.max(0) as usize);
    let ax_tid = unsafe { AXUIElementGetTypeID() };
    for i in 0..count {
        let item = unsafe { CFArrayGetValueAtIndex(ptr, i) };
        if !item.is_null() && unsafe { CFGetTypeID(item) == ax_tid } {
            if let Some(owned) = unsafe { CfRef::from_borrowed(item) } {
                out.push(owned);
            }
        }
    }
    out
}

fn cf_to_strings(ptr: CFTypeRef) -> Vec<String> {
    if ptr.is_null() || unsafe { CFGetTypeID(ptr) != CFArrayGetTypeID() } {
        return Vec::new();
    }
    let count = unsafe { CFArrayGetCount(ptr) };
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count {
        let item = unsafe { CFArrayGetValueAtIndex(ptr, i) };
        if let Some(s) = cf_to_string(item) {
            out.push(s);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Objective-C helpers for NSRunningApplication, ScreenCaptureKit, Vision
// ---------------------------------------------------------------------------

unsafe fn cls(name: &str) -> *mut c_void {
    let c = CString::new(name).unwrap();
    objc_getClass(c.as_ptr())
}

unsafe fn sel(name: &str) -> *mut c_void {
    let c = CString::new(name).unwrap();
    sel_registerName(c.as_ptr())
}

fn find_kakaotalk_pid() -> Result<i32> {
    unsafe {
        let running_app_cls = cls("NSRunningApplication");
        let bundle = cf_string("com.kakao.KakaoTalkMac");
        let f: extern "C" fn(*mut c_void, *mut c_void, CFStringRef) -> CFArrayRef =
            std::mem::transmute(objc_msgSend as *const ());
        let arr = f(
            running_app_cls,
            sel("runningApplicationsWithBundleIdentifier:"),
            bundle.as_ptr(),
        );
        if arr.is_null() || CFArrayGetCount(arr) == 0 {
            return Err(KaxError::blocked("app_not_running"));
        }
        let app = CFArrayGetValueAtIndex(arr, 0) as *mut c_void;
        let pid_fn: extern "C" fn(*mut c_void, *mut c_void) -> i32 =
            std::mem::transmute(objc_msgSend as *const ());
        Ok(pid_fn(app, sel("processIdentifier")))
    }
}

fn ensure_ns_application_shared() {
    unsafe {
        let ns_app = cls("NSApplication");
        let f: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let _ = f(ns_app, sel("sharedApplication"));
    }
}

// Load ScreenCaptureKit only for attachment capture, so ordinary AX commands can
// start on macOS releases that do not ship the framework.
fn attachment_capture_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| unsafe {
        #[repr(C)]
        struct OperatingSystemVersion {
            major: isize,
            minor: isize,
            patch: isize,
        }
        let process_info: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let at_least: extern "C" fn(*mut c_void, *mut c_void, OperatingSystemVersion) -> u8 =
            std::mem::transmute(objc_msgSend as *const ());
        let info = process_info(cls("NSProcessInfo"), sel("processInfo"));
        let required = OperatingSystemVersion { major: 14, minor: 2, patch: 0 };
        if at_least(info, sel("isOperatingSystemAtLeastVersion:"), required) == 0 {
            return false;
        }
        // Objective-C classes and asynchronous capture callbacks live for the process.
        !libc::dlopen(
            b"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit\0".as_ptr().cast(),
            libc::RTLD_LOCAL | libc::RTLD_LAZY,
        ).is_null()
    })
}

unsafe fn sc_window_frame(sc_win: *mut c_void) -> CGRect {
    let s = sel("frame");
    #[cfg(target_arch = "x86_64")]
    {
        let f: extern "C" fn(*mut CGRect, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend_stret as *const ());
        let mut rect = CGRect::default();
        f(&mut rect, sc_win, s);
        rect
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let f: extern "C" fn(*mut c_void, *mut c_void) -> CGRect =
            std::mem::transmute(objc_msgSend as *const ());
        f(sc_win, s)
    }
}

fn encode_cgimage_png(image: CGImageRef) -> Result<Vec<u8>> {
    unsafe {
        let data = CFDataCreateMutable(std::ptr::null(), 0);
        if data.is_null() {
            return Err(KaxError::blocked("attachment_png_failed"));
        }
        let data_guard = CfRef(data as CFTypeRef);
        let uti = cf_string("public.png");
        let dest = CGImageDestinationCreateWithData(data, uti.as_ptr(), 1, std::ptr::null());
        if dest.is_null() {
            return Err(KaxError::blocked("attachment_png_failed"));
        }
        let dest_guard = CfRef(dest as CFTypeRef);
        CGImageDestinationAddImage(dest, image, std::ptr::null());
        let ok = CGImageDestinationFinalize(dest);
        drop(dest_guard);
        if ok == 0 {
            return Err(KaxError::blocked("attachment_png_failed"));
        }
        let len = CFDataGetLength(data_guard.as_ptr()) as usize;
        let ptr = CFDataGetBytePtr(data_guard.as_ptr());
        if ptr.is_null() || len == 0 {
            return Err(KaxError::blocked("attachment_png_failed"));
        }
        Ok(std::slice::from_raw_parts(ptr, len).to_vec())
    }
}

fn vision_ocr(image: CGImageRef) -> Result<Value> {
    unsafe {
        let req_cls = cls("VNRecognizeTextRequest");
        let alloc_fn: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let req = alloc_fn(alloc_fn(req_cls, sel("alloc")), sel("init"));
        if req.is_null() {
            return Err(KaxError::blocked("vision_ocr_unavailable"));
        }
        let _req_guard = CfRef(req as CFTypeRef);

        // recognitionLevel = VNRequestTextRecognitionLevelAccurate (0)
        let set_level: extern "C" fn(*mut c_void, *mut c_void, isize) =
            std::mem::transmute(objc_msgSend as *const ());
        set_level(req, sel("setRecognitionLevel:"), 0);

        let ko = cf_string("ko-KR");
        let en = cf_string("en-US");
        let lang_vals = [ko.as_ptr(), en.as_ptr()];
        let langs = CfRef(CFArrayCreate(
            std::ptr::null(),
            lang_vals.as_ptr(),
            2,
            std::ptr::null(),
        ));
        let set_langs: extern "C" fn(*mut c_void, *mut c_void, CFArrayRef) =
            std::mem::transmute(objc_msgSend as *const ());
        set_langs(req, sel("setRecognitionLanguages:"), langs.as_ptr());

        let set_corr: extern "C" fn(*mut c_void, *mut c_void, u8) =
            std::mem::transmute(objc_msgSend as *const ());
        set_corr(req, sel("setUsesLanguageCorrection:"), 0);

        let handler_cls = cls("VNImageRequestHandler");
        let h_alloc = alloc_fn(handler_cls, sel("alloc"));
        let dict_cls = cls("NSDictionary");
        let empty_dict = alloc_fn(dict_cls, sel("dictionary"));
        let init_handler: extern "C" fn(
            *mut c_void,
            *mut c_void,
            CGImageRef,
            *mut c_void,
        ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
        let handler = init_handler(h_alloc, sel("initWithCGImage:options:"), image, empty_dict);
        if handler.is_null() {
            return Err(KaxError::blocked("vision_ocr_failed"));
        }
        let _handler_guard = CfRef(handler as CFTypeRef);

        let req_arr_vals = [req as CFTypeRef];
        let req_arr = CfRef(CFArrayCreate(
            std::ptr::null(),
            req_arr_vals.as_ptr(),
            1,
            std::ptr::null(),
        ));
        let mut err: *mut c_void = std::ptr::null_mut();
        let perform: extern "C" fn(*mut c_void, *mut c_void, CFArrayRef, *mut *mut c_void) -> u8 =
            std::mem::transmute(objc_msgSend as *const ());
        if perform(handler, sel("performRequests:error:"), req_arr.as_ptr(), &mut err) == 0 {
            return Err(KaxError::blocked("vision_ocr_failed"));
        }

        let get_results: extern "C" fn(*mut c_void, *mut c_void) -> CFArrayRef =
            std::mem::transmute(objc_msgSend as *const ());
        let results = get_results(req, sel("results"));
        let mut lines = Vec::new();
        let mut joined = Vec::new();
        if !results.is_null() {
            let count = CFArrayGetCount(results);
            let top_cand: extern "C" fn(*mut c_void, *mut c_void, usize) -> CFArrayRef =
                std::mem::transmute(objc_msgSend as *const ());
            let get_str: extern "C" fn(*mut c_void, *mut c_void) -> CFStringRef =
                std::mem::transmute(objc_msgSend as *const ());
            let get_conf: extern "C" fn(*mut c_void, *mut c_void) -> f32 =
                std::mem::transmute(objc_msgSend as *const ());
            for i in 0..count {
                let obs = CFArrayGetValueAtIndex(results, i) as *mut c_void;
                let cands = top_cand(obs, sel("topCandidates:"), 1);
                if !cands.is_null() && CFArrayGetCount(cands) > 0 {
                    let first = CFArrayGetValueAtIndex(cands, 0) as *mut c_void;
                    let text = cf_to_string(get_str(first, sel("string"))).unwrap_or_default();
                    let conf = get_conf(first, sel("confidence"));
                    joined.push(text.clone());
                    lines.push(json!({"text": text, "confidence": conf}));
                }
            }
        }
        Ok(json!({
            "text": joined.join("\n"),
            "lines": lines,
            "source": "vision_ocr",
            "mayBeInaccurate": true,
        }))
    }
}

// ---------------------------------------------------------------------------
// IdentitySet for AXUIElement deduplication (CFHash + CFEqual)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct IdentitySet {
    buckets: HashMap<CFHashCode, Vec<CfRef>>,
}

impl IdentitySet {
    fn insert(&mut self, el: &CfRef) -> bool {
        let key = unsafe { CFHash(el.as_ptr()) };
        let entry = self.buckets.entry(key).or_default();
        if entry
            .iter()
            .any(|existing| unsafe { CFEqual(existing.as_ptr(), el.as_ptr()) != 0 })
        {
            return false;
        }
        entry.push(el.clone());
        true
    }
}

fn ax_equal(a: &CfRef, b: &CfRef) -> bool {
    unsafe { CFEqual(a.as_ptr(), b.as_ptr()) != 0 }
}

// ---------------------------------------------------------------------------
// Live AX Contexts: Read, Control, Send
// ---------------------------------------------------------------------------

/// Node budget for one read pass: generous enough for `SEARCH_ROW_CAP` rows of ~5-10 nodes.
const MAX_READ_NODES: usize = 100_000;

struct ReadCtx {
    started: Instant,
    /// Stall deadline: pushed forward by [`ReadCtx::progress`] after every transcript row.
    deadline: Instant,
    /// Hard deadline of the whole tool call (`KAX_CALL_TIMEOUT_SECS`); never extended.
    hard_deadline: Instant,
    ui_actions: usize,
    nodes_visited: usize,
    text_bytes: usize,
    optional_failures: HashMap<String, usize>,
    /// Add on-screen `frame`s ([x, y, w, h]) to row, text, image and button parts.
    collect_frames: bool,
}

impl ReadCtx {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            deadline: now + read_timeout(),
            hard_deadline: call_deadline().unwrap_or_else(|| now + call_timeout()),
            ui_actions: 0,
            nodes_visited: 0,
            text_bytes: 0,
            optional_failures: HashMap::new(),
            collect_frames: false,
        }
    }

    fn check_budget(&self) -> Result<()> {
        let now = Instant::now();
        if now >= self.hard_deadline {
            return Err(KaxError::blocked("tool_call_time_limit"));
        }
        if now >= self.deadline
            || self.nodes_visited > MAX_READ_NODES
            || self.text_bytes > 16 * 1024 * 1024
        {
            return Err(KaxError::blocked("accessibility_read_limit"));
        }
        Ok(())
    }

    /// A transcript row finished: restart the stall timer.
    fn progress(&mut self) {
        self.deadline = Instant::now() + read_timeout();
    }

    fn attribute(&mut self, el: &CfRef, name: &str, optional: bool) -> Result<Option<CfRef>> {
        self.check_budget()?;
        let attr = cf_string(name);
        let mut val: CFTypeRef = std::ptr::null();
        let rc = unsafe { AXUIElementCopyAttributeValue(el.as_ptr(), attr.as_ptr(), &mut val) };
        if rc == K_AX_ERROR_SUCCESS {
            return Ok(unsafe { CfRef::from_owned(val) });
        }
        if matches!(
            rc,
            K_AX_ERROR_ATTRIBUTE_UNSUPPORTED | K_AX_ERROR_NO_VALUE | K_AX_ERROR_NOT_IMPLEMENTED
        ) {
            return Ok(None);
        }
        if rc == K_AX_ERROR_FAILURE && optional {
            *self.optional_failures.entry(name.to_string()).or_insert(0) += 1;
            return Ok(None);
        }
        Err(KaxError::blocked(format!(
            "accessibility_read_failed_{name}_{rc}"
        )))
    }

    fn string(&mut self, el: &CfRef, name: &str, optional: bool) -> Result<String> {
        Ok(self
            .attribute(el, name, optional)?
            .and_then(|v| cf_to_string(v.as_ptr()))
            .unwrap_or_default())
    }

    fn elements(&mut self, el: &CfRef, name: &str) -> Result<Vec<CfRef>> {
        Ok(self
            .attribute(el, name, false)?
            .map(|v| cf_to_elements(v.as_ptr()))
            .unwrap_or_default())
    }

    /// One batched IPC for everything `walk_row` needs from a node. Off-screen transcript rows
    /// cost ~5 ms per round trip (KakaoTalk materialises the cell view for every request), so
    /// `AXValue` rides in the same batch instead of a second call. Its error, if any, is
    /// returned separately because whether it is fatal depends on the role.
    fn node_attributes(
        &mut self,
        el: &CfRef,
    ) -> Result<(HashMap<&'static str, CfRef>, Option<AXError>)> {
        self.check_budget()?;
        const NAMES: &[&str] =
            &["AXRole", "AXTitle", "AXDescription", "AXHelp", "AXChildren", "AXValue"];
        let cf_names: Vec<CfRef> = NAMES.iter().map(|n| cf_string(n)).collect();
        let ptrs: Vec<CFTypeRef> = cf_names.iter().map(|c| c.as_ptr()).collect();
        let arr = CfRef(unsafe {
            CFArrayCreate(
                std::ptr::null(),
                ptrs.as_ptr(),
                ptrs.len() as CFIndex,
                std::ptr::null(),
            )
        });
        let mut copied: CFArrayRef = std::ptr::null();
        let rc = unsafe {
            AXUIElementCopyMultipleAttributeValues(el.as_ptr(), arr.as_ptr(), 0, &mut copied)
        };
        if rc != K_AX_ERROR_SUCCESS || copied.is_null() {
            return Err(KaxError::blocked(format!(
                "accessibility_batch_read_failed_{rc}"
            )));
        }
        let copied_guard = CfRef(copied);
        let count = unsafe { CFArrayGetCount(copied_guard.as_ptr()) } as usize;
        if count != NAMES.len() {
            return Err(KaxError::blocked(format!(
                "accessibility_batch_read_failed_{rc}"
            )));
        }
        let null_tid = unsafe { CFNullGetTypeID() };
        let axval_tid = unsafe { AXValueGetTypeID() };
        let mut out = HashMap::new();
        let mut value_err = None;
        for (idx, &name) in NAMES.iter().enumerate() {
            let v = unsafe { CFArrayGetValueAtIndex(copied_guard.as_ptr(), idx as CFIndex) };
            if v.is_null() || unsafe { CFGetTypeID(v) == null_tid } {
                continue;
            }
            if unsafe { CFGetTypeID(v) == axval_tid } && unsafe { AXValueGetType(v) == K_AX_VALUE_AXERROR_TYPE } {
                let mut err: AXError = 0;
                if unsafe {
                    AXValueGetValue(v, K_AX_VALUE_AXERROR_TYPE, (&mut err as *mut AXError).cast()) == 0
                } {
                    return Err(KaxError::blocked("invalid_accessibility_batch_error"));
                }
                if matches!(
                    err,
                    K_AX_ERROR_ATTRIBUTE_UNSUPPORTED
                        | K_AX_ERROR_NO_VALUE
                        | K_AX_ERROR_NOT_IMPLEMENTED
                ) {
                    continue;
                }
                if name == "AXValue" {
                    value_err = Some(err);
                    continue;
                }
                if matches!(name, "AXTitle" | "AXDescription" | "AXHelp") && err == K_AX_ERROR_FAILURE {
                    *self.optional_failures.entry(name.to_string()).or_insert(0) += 1;
                    continue;
                }
                return Err(KaxError::blocked(format!(
                    "accessibility_read_failed_{name}_{err}"
                )));
            }
            if let Some(owned) = unsafe { CfRef::from_borrowed(v) } {
                out.insert(name, owned);
            }
        }
        Ok((out, value_err))
    }

    fn controls(&mut self, window: &CfRef) -> Result<(usize, Vec<CfRef>)> {
        if !self.elements(window, "AXSheets")?.is_empty() {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        let mut seen = IdentitySet::default();
        let mut composers = 0usize;
        let mut tables = Vec::new();
        self.walk_controls(window, 0, &mut seen, &mut composers, &mut tables)?;
        Ok((composers, tables))
    }

    fn walk_controls(
        &mut self,
        el: &CfRef,
        depth: usize,
        seen: &mut IdentitySet,
        composers: &mut usize,
        tables: &mut Vec<CfRef>,
    ) -> Result<()> {
        if !seen.insert(el) {
            return Ok(());
        }
        self.nodes_visited += 1;
        self.check_budget()?;
        if depth > 16 {
            return Err(KaxError::blocked("accessibility_tree_depth"));
        }
        let role = self.string(el, "AXRole", false)?;
        if matches!(role.as_str(), "AXSheet" | "AXDialog") {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        if role == "AXTable" {
            tables.push(el.clone());
            return Ok(());
        }
        // Popovers (e.g. the member list opened from "대화상대 N명") hold their own tables and
        // fields; they are not part of the chat window's transcript or controls.
        if matches!(role.as_str(), "AXRow" | "AXCell" | "AXPopover") {
            return Ok(());
        }
        if role == "AXTextArea" {
            let desc = self.string(el, "AXDescription", true)?;
            if COMPOSER_LABELS.contains(&desc.as_str()) {
                *composers += 1;
            }
            return Ok(());
        }
        for child in self.elements(el, "AXChildren")? {
            self.walk_controls(&child, depth + 1, seen, composers, tables)?;
        }
        Ok(())
    }

    /// The newest `cap` transcript rows: `(rows, total exposed, index of first returned, source)`.
    fn rows(&mut self, table: &CfRef, cap: usize) -> Result<(Vec<CfRef>, usize, usize, &'static str)> {
        self.check_budget()?;
        let attr = cf_string("AXRows");
        let mut count: CFIndex = 0;
        let rc = unsafe { AXUIElementGetAttributeValueCount(table.as_ptr(), attr.as_ptr(), &mut count) };
        if rc == K_AX_ERROR_SUCCESS {
            let count_u = count.max(0) as usize;
            let start = count_u.saturating_sub(cap);
            if count_u == 0 {
                return Ok((Vec::new(), 0, 0, "AXRows"));
            }
            let want = count_u - start;
            let mut val: CFArrayRef = std::ptr::null();
            let copied = unsafe {
                AXUIElementCopyAttributeValues(
                    table.as_ptr(),
                    attr.as_ptr(),
                    start as CFIndex,
                    want as CFIndex,
                    &mut val,
                )
            };
            if copied != K_AX_ERROR_SUCCESS || val.is_null() {
                return Err(KaxError::blocked("transcript_rows_unavailable"));
            }
            let guard = CfRef(val);
            let items = cf_to_elements(guard.as_ptr());
            if items.len() != want {
                return Err(KaxError::blocked("transcript_rows_unavailable"));
            }
            return Ok((items, count_u, start, "AXRows"));
        }
        if !matches!(
            rc,
            K_AX_ERROR_ATTRIBUTE_UNSUPPORTED | K_AX_ERROR_NO_VALUE | K_AX_ERROR_NOT_IMPLEMENTED
        ) {
            return Err(KaxError::blocked("transcript_rows_unavailable"));
        }
        let mut children = Vec::new();
        for c in self.elements(table, "AXChildren")? {
            if self.string(&c, "AXRole", false)? == "AXRow" {
                children.push(c);
            }
        }
        let count_u = children.len();
        let start = count_u.saturating_sub(cap);
        Ok((children.split_off(start), count_u, start, "AXChildren"))
    }

    fn read_row(&mut self, row: &CfRef, index: usize) -> Result<Value> {
        self.read_row_checked(row, index, false)
    }

    /// `require_row`: fail with `invalid_transcript_rows` unless `row`'s role is `AXRow`, using
    /// the role from the row's own batched read (no separate round trip).
    fn read_row_checked(&mut self, row: &CfRef, index: usize, require_row: bool) -> Result<Value> {
        let mut seen = IdentitySet::default();
        let mut parts: Vec<Value> = Vec::new();
        self.walk_row(row, 0, require_row, &mut seen, &mut parts)?;
        Ok(json!({"rowIndex": index, "parts": parts}))
    }

    fn walk_row(
        &mut self,
        el: &CfRef,
        depth: usize,
        require_row: bool,
        seen: &mut IdentitySet,
        parts: &mut Vec<Value>,
    ) -> Result<()> {
        if !seen.insert(el) {
            return Ok(());
        }
        self.nodes_visited += 1;
        self.check_budget()?;
        if depth > 16 {
            return Err(KaxError::blocked("accessibility_tree_depth"));
        }
        let (attrs, value_err) = self.node_attributes(el)?;
        let role = attrs
            .get("AXRole")
            .and_then(|v| cf_to_string(v.as_ptr()))
            .unwrap_or_default();
        if require_row && role != "AXRow" {
            return Err(KaxError::blocked("invalid_transcript_rows"));
        }
        if role.is_empty() {
            return Err(KaxError::blocked("accessibility_role_unavailable"));
        }
        let mut part = Map::new();
        part.insert("role".to_string(), json!(role));
        let desc = attrs
            .get("AXDescription")
            .and_then(|v| cf_to_string(v.as_ptr()))
            .unwrap_or_default();
        if role == "AXTextArea" && COMPOSER_LABELS.contains(&desc.as_str()) {
            return Err(KaxError::blocked("composer_inside_transcript"));
        }
        if !desc.is_empty() {
            self.text_bytes += desc.len();
            part.insert("AXDescription".to_string(), json!(desc));
        }
        if !matches!(role.as_str(), "AXRow" | "AXCell") {
            // Same rules as the former separate read: required for text roles, otherwise only
            // kAXErrorFailure is tolerated (and counted).
            let required = matches!(role.as_str(), "AXTextArea" | "AXStaticText");
            if let Some(err) = value_err {
                if required || err != K_AX_ERROR_FAILURE {
                    return Err(KaxError::blocked(format!("accessibility_read_failed_AXValue_{err}")));
                }
                *self.optional_failures.entry("AXValue".to_string()).or_insert(0) += 1;
            }
            let val = attrs
                .get("AXValue")
                .and_then(|v| cf_to_string(v.as_ptr()))
                .unwrap_or_default();
            if !val.is_empty() {
                self.text_bytes += val.len();
                part.insert("AXValue".to_string(), json!(val));
            }
        }
        for name in ["AXTitle", "AXHelp"] {
            let text = attrs
                .get(name)
                .and_then(|v| cf_to_string(v.as_ptr()))
                .unwrap_or_default();
            if !text.is_empty() {
                self.text_bytes += text.len();
                part.insert(name.to_string(), json!(text));
            }
        }
        if self.collect_frames && matches!(role.as_str(), "AXRow" | "AXTextArea" | "AXImage" | "AXButton") {
            if let Ok(r) = self.frame(el) {
                part.insert(
                    "frame".to_string(),
                    json!([r.origin.x, r.origin.y, r.size.width, r.size.height]),
                );
            }
        }
        self.check_budget()?;
        if part.len() > 1 || matches!(role.as_str(), "AXImage" | "AXLink") {
            parts.push(Value::Object(part));
        }
        if role == "AXTextArea" {
            return Ok(());
        }
        if let Some(children_ref) = attrs.get("AXChildren") {
            for child in cf_to_elements(children_ref.as_ptr()) {
                self.walk_row(&child, depth + 1, false, seen, parts)?;
            }
        }
        Ok(())
    }

    /// `AXFrame` in one request (off-screen transcript rows answer each request slowly).
    fn ax_frame(&mut self, el: &CfRef) -> Option<CGRect> {
        let v = self.attribute(el, "AXFrame", true).ok().flatten()?;
        if unsafe { CFGetTypeID(v.as_ptr()) != AXValueGetTypeID() } {
            return None;
        }
        let mut r = CGRect { origin: CGPoint::default(), size: CGSize::default() };
        let ok = unsafe { AXValueGetValue(v.as_ptr(), K_AX_VALUE_CGRECT_TYPE, (&mut r as *mut CGRect).cast()) };
        (ok != 0).then_some(r)
    }

    /// The element carrying a row's message: the body text, image, file group or a described
    /// button (video), never the profile button.
    fn message_element(&mut self, row: &CfRef) -> Option<CfRef> {
        for cell in self.elements(row, "AXChildren").ok()? {
            for child in self.elements(&cell, "AXChildren").ok()? {
                let role = self.string(&child, "AXRole", true).ok()?;
                let desc = self.string(&child, "AXDescription", true).unwrap_or_default();
                let fits = match role.as_str() {
                    "AXTextArea" | "AXImage" => true,
                    "AXGroup" => desc == "파일",
                    "AXButton" => !desc.is_empty() && desc != "프로필",
                    _ => false,
                };
                if fits {
                    return Some(child);
                }
            }
        }
        None
    }

    fn frame(&mut self, el: &CfRef) -> Result<CGRect> {
        let Some(pos) = self.attribute(el, "AXPosition", false)? else {
            return Err(KaxError::blocked("attachment_geometry_unavailable"));
        };
        let Some(size) = self.attribute(el, "AXSize", false)? else {
            return Err(KaxError::blocked("attachment_geometry_unavailable"));
        };
        let axval_tid = unsafe { AXValueGetTypeID() };
        if unsafe { CFGetTypeID(pos.as_ptr()) != axval_tid || CFGetTypeID(size.as_ptr()) != axval_tid }
        {
            return Err(KaxError::blocked("attachment_geometry_unavailable"));
        }
        let mut pt = CGPoint::default();
        let mut sz = CGSize::default();
        let ok1 = unsafe {
            AXValueGetValue(
                pos.as_ptr(),
                K_AX_VALUE_CGPOINT_TYPE,
                (&mut pt as *mut CGPoint).cast(),
            )
        };
        let ok2 = unsafe {
            AXValueGetValue(
                size.as_ptr(),
                K_AX_VALUE_CGSIZE_TYPE,
                (&mut sz as *mut CGSize).cast(),
            )
        };
        if ok1 == 0 || ok2 == 0 {
            return Err(KaxError::blocked("attachment_geometry_unavailable"));
        }
        Ok(CGRect {
            origin: pt,
            size: sz,
        })
    }

    fn image_elements(&mut self, root: &CfRef) -> Result<Vec<CfRef>> {
        let mut seen = IdentitySet::default();
        let mut out = Vec::new();
        self.walk_images(root, 0, &mut seen, &mut out)?;
        Ok(out)
    }

    fn walk_images(
        &mut self,
        el: &CfRef,
        depth: usize,
        seen: &mut IdentitySet,
        out: &mut Vec<CfRef>,
    ) -> Result<()> {
        if !seen.insert(el) {
            return Ok(());
        }
        self.nodes_visited += 1;
        self.check_budget()?;
        if depth > 16 {
            return Err(KaxError::blocked("accessibility_tree_depth"));
        }
        let role = self.string(el, "AXRole", false)?;
        if role == "AXImage" {
            out.push(el.clone());
            return Ok(());
        }
        if role == "AXTextArea" {
            return Ok(());
        }
        for child in self.elements(el, "AXChildren")? {
            self.walk_images(&child, depth + 1, seen, out)?;
        }
        Ok(())
    }

    fn finalize_report(&self, mut report: Map<String, Value>) -> Value {
        report.insert("source".to_string(), json!("macos_accessibility"));
        report.insert("attributeReadMode".to_string(), json!("batched_metadata"));
        report.insert("databaseCalls".to_string(), json!(0));
        report.insert("uiActions".to_string(), json!(self.ui_actions));
        report.insert(
            "optionalAttributeFailures".to_string(),
            json!(self.optional_failures),
        );
        report.insert("dispatchAttempted".to_string(), json!(false));
        report.insert("nodesVisited".to_string(), json!(self.nodes_visited));
        report.insert(
            "elapsedMs".to_string(),
            json!(self.started.elapsed().as_millis() as u64),
        );
        Value::Object(report)
    }
}

/// How long a newly opened window must keep the composer-less shape before `open` treats it
/// as a one-way channel window rather than a chat whose composer is still being built.
const ONE_WAY_WINDOW_SETTLE: Duration = Duration::from_millis(500);

/// One tab of the room-list folder bar.
struct RoomFolder {
    group: CfRef,
    name: String,
    /// `None` when the tab shows a count that is not a plain number.
    unread: Option<u64>,
}

struct ControlCtx {
    started: Instant,
    deadline: Instant,
    visits: usize,
    actions: Vec<Value>,
}

impl ControlCtx {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            deadline: now + Duration::from_secs(8),
            visits: 0,
            actions: Vec::new(),
        }
    }

    fn check(&self) -> Result<()> {
        if self.visits >= 20_000 || Instant::now() >= self.deadline {
            return Err(KaxError::blocked("accessibility_control_limit"));
        }
        Ok(())
    }

    fn attr(el: &CfRef, name: &str) -> Option<CfRef> {
        let attr = cf_string(name);
        let mut val: CFTypeRef = std::ptr::null();
        let rc = unsafe { AXUIElementCopyAttributeValue(el.as_ptr(), attr.as_ptr(), &mut val) };
        if rc == K_AX_ERROR_SUCCESS {
            unsafe { CfRef::from_owned(val) }
        } else {
            None
        }
    }

    fn string(el: &CfRef, name: &str) -> String {
        Self::attr(el, name)
            .and_then(|v| cf_to_string(v.as_ptr()))
            .unwrap_or_default()
    }

    fn elements(el: &CfRef, name: &str) -> Vec<CfRef> {
        Self::attr(el, name)
            .map(|v| cf_to_elements(v.as_ptr()))
            .unwrap_or_default()
    }

    fn composer_text(el: &CfRef) -> Result<String> {
        let attr = cf_string("AXValue");
        let mut val: CFTypeRef = std::ptr::null();
        let rc = unsafe { AXUIElementCopyAttributeValue(el.as_ptr(), attr.as_ptr(), &mut val) };
        if rc == K_AX_ERROR_NO_VALUE {
            return Ok(String::new());
        }
        if rc != K_AX_ERROR_SUCCESS {
            return Err(KaxError::blocked("composer_value_unavailable"));
        }
        let guard = unsafe { CfRef::from_owned(val) }
            .ok_or_else(|| KaxError::blocked("composer_value_unavailable"))?;
        cf_to_string(guard.as_ptr()).ok_or_else(|| KaxError::blocked("composer_value_unavailable"))
    }

    fn action_names(el: &CfRef) -> Vec<String> {
        let mut arr: CFArrayRef = std::ptr::null();
        let rc = unsafe { AXUIElementCopyActionNames(el.as_ptr(), &mut arr) };
        if rc != K_AX_ERROR_SUCCESS || arr.is_null() {
            return Vec::new();
        }
        let guard = CfRef(arr);
        cf_to_strings(guard.as_ptr())
    }

    fn settable(el: &CfRef, name: &str) -> bool {
        let attr = cf_string(name);
        let mut ok: u8 = 0;
        unsafe { AXUIElementIsAttributeSettable(el.as_ptr(), attr.as_ptr(), &mut ok) == K_AX_ERROR_SUCCESS && ok != 0 }
    }

    fn act(&mut self, el: &CfRef, name: &str) -> Result<()> {
        self.check()?;
        let act_name = cf_string(name);
        let rc = unsafe { AXUIElementPerformAction(el.as_ptr(), act_name.as_ptr()) };
        self.actions.push(json!({"action": name, "result": rc}));
        Ok(())
    }

    fn set_search_value(&mut self, el: &CfRef, value: &str) -> Result<()> {
        self.check()?;
        let role = Self::string(el, "AXRole");
        if !matches!(role.as_str(), "AXTextField" | "AXSearchField") {
            return Err(KaxError::blocked("only_search_text_fields_may_be_edited"));
        }
        if Self::settable(el, "AXFocused") {
            let attr = cf_string("AXFocused");
            let rc = unsafe { AXUIElementSetAttributeValue(el.as_ptr(), attr.as_ptr(), kCFBooleanTrue) };
            self.actions
                .push(json!({"action": "AXFocused(search)", "result": rc}));
        }
        if Self::settable(el, "AXSelectedText") && Self::settable(el, "AXSelectedTextRange") {
            let cur_len = Self::string(el, "AXValue").encode_utf16().count() as CFIndex;
            let range = CFRange {
                location: 0,
                length: cur_len,
            };
            let range_val = unsafe {
                CfRef::from_owned(AXValueCreate(
                    K_AX_VALUE_CFRANGE_TYPE,
                    (&range as *const CFRange).cast(),
                ))
            }
            .ok_or_else(|| KaxError::blocked("search_selection_unavailable"))?;
            let sel_range_attr = cf_string("AXSelectedTextRange");
            let rc_sel = unsafe {
                AXUIElementSetAttributeValue(el.as_ptr(), sel_range_attr.as_ptr(), range_val.as_ptr())
            };
            self.actions
                .push(json!({"action": "AXSelectedTextRange(search)", "result": rc_sel}));
            if rc_sel != K_AX_ERROR_SUCCESS {
                return Err(KaxError::blocked("search_selection_failed"));
            }
            let val_cf = cf_string(value);
            let sel_text_attr = cf_string("AXSelectedText");
            let rc_edit = unsafe {
                AXUIElementSetAttributeValue(el.as_ptr(), sel_text_attr.as_ptr(), val_cf.as_ptr())
            };
            self.actions
                .push(json!({"action": "AXSelectedText(search)", "result": rc_edit}));
            if rc_edit != K_AX_ERROR_SUCCESS || Self::string(el, "AXValue") != value {
                return Err(KaxError::blocked("search_text_edit_failed"));
            }
            return Ok(());
        }
        Err(KaxError::blocked("search_text_edit_not_supported"))
    }

    fn nodes(&mut self, el: &CfRef, tables: bool, depth: usize) -> Result<Vec<CfRef>> {
        self.visits += 1;
        self.check()?;
        if depth >= 20 {
            return Err(KaxError::blocked("accessibility_tree_depth"));
        }
        let role = Self::string(el, "AXRole");
        if matches!(role.as_str(), "AXSheet" | "AXDialog") {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        // A popover below the scan root (e.g. the member list) is not part of the window's own
        // controls: its search field and table must not count as the window's.
        if depth > 0 && role == "AXPopover" {
            return Ok(Vec::new());
        }
        let mut out = vec![el.clone()];
        if !tables && matches!(role.as_str(), "AXTable" | "AXOutline" | "AXRow" | "AXCell") {
            return Ok(out);
        }
        if matches!(role.as_str(), "AXTextArea" | "AXTextField" | "AXSearchField") {
            return Ok(out);
        }
        for child in Self::elements(el, "AXChildren") {
            out.extend(self.nodes(&child, tables, depth + 1)?);
        }
        Ok(out)
    }

    /// `AXRows` count of a table without copying the rows, or `None` if it cannot be read.
    fn reported_row_count(table: &CfRef) -> Option<usize> {
        let mut reported: CFIndex = 0;
        let attr = cf_string("AXRows");
        let rc = unsafe { AXUIElementGetAttributeValueCount(table.as_ptr(), attr.as_ptr(), &mut reported) };
        (rc == K_AX_ERROR_SUCCESS).then_some(reported.max(0) as usize)
    }

    fn chat_controls(&mut self, window: &CfRef) -> Result<Vec<CfRef>> {
        Ok(self.chat_window_shape(window)?.0)
    }

    /// The window's controls and whether it has a composer (`false`: one-way channel window).
    fn chat_window_shape(&mut self, window: &CfRef) -> Result<(Vec<CfRef>, bool)> {
        if !Self::elements(window, "AXSheets").is_empty() {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        let found = self.nodes(window, false, 0)?;
        let composers = found
            .iter()
            .filter(|n| {
                Self::string(n, "AXRole") == "AXTextArea"
                    && COMPOSER_LABELS.contains(&Self::string(n, "AXDescription").as_str())
            })
            .count();
        let tables = found
            .iter()
            .filter(|n| Self::string(n, "AXRole") == "AXTable")
            .count();
        let has_composer = chat_window_check(composers, tables).map_err(KaxError::blocked)?;
        Ok((found, has_composer))
    }

    fn window(&mut self, application: &CfRef, title: &str) -> Result<CfRef> {
        Ok(self.window_shape(application, title)?.0)
    }

    /// Like [`Self::window`], also saying whether the window has a composer.
    fn window_shape(&mut self, application: &CfRef, title: &str) -> Result<(CfRef, bool)> {
        let target = Self::titled_window(application, title)?;
        let (_, has_composer) = self.chat_window_shape(&target)?;
        Ok((target, has_composer))
    }

    /// The unique non-main window titled `title`, without checking what it contains.
    fn titled_window(application: &CfRef, title: &str) -> Result<CfRef> {
        let matches: Vec<CfRef> = Self::elements(application, "AXWindows")
            .into_iter()
            .filter(|w| Self::string(w, "AXTitle") == title)
            .collect();
        if matches.len() != 1 {
            return Err(KaxError::blocked(if matches.is_empty() {
                "target_room_window_not_open"
            } else {
                "target_room_window_ambiguous"
            }));
        }
        if Self::string(&matches[0], "AXIdentifier") == "Main Window" {
            return Err(KaxError::blocked("main_window_is_not_a_chat"));
        }
        Ok(matches.into_iter().next().unwrap())
    }

    fn main_window(application: &CfRef) -> Result<CfRef> {
        let found: Vec<CfRef> = Self::elements(application, "AXWindows")
            .into_iter()
            .filter(|w| Self::string(w, "AXIdentifier") == "Main Window")
            .collect();
        if found.len() != 1 {
            return Err(KaxError::blocked("main_window_not_unique"));
        }
        Ok(found.into_iter().next().unwrap())
    }

    fn pane_matches(main: &CfRef, pane_labels: &[&str]) -> bool {
        Self::elements(main, "AXChildren").iter().any(|c| {
            Self::string(c, "AXRole") == "AXStaticText"
                && pane_labels.contains(&Self::string(c, "AXValue").as_str())
        })
    }

    fn ensure_pane(
        &mut self,
        application: &CfRef,
        pane_labels: &[&str],
        unavailable_reason: &str,
        not_unique_reason: &str,
        not_observed_reason: &str,
    ) -> Result<CfRef> {
        let main = Self::main_window(application)?;
        if Self::pane_matches(&main, pane_labels) {
            return Ok(main);
        }
        let bar = Self::attr(application, "AXMenuBar")
            .ok_or_else(|| KaxError::blocked(unavailable_reason))?;
        let top: Vec<CfRef> = Self::elements(&bar, "AXChildren")
            .into_iter()
            .filter(|c| matches!(Self::string(c, "AXTitle").as_str(), "창" | "Window"))
            .collect();
        let menus: Vec<CfRef> = top
            .iter()
            .flat_map(|t| Self::elements(t, "AXChildren"))
            .collect();
        let items: Vec<CfRef> = menus
            .iter()
            .flat_map(|m| Self::elements(m, "AXChildren"))
            .filter(|i| {
                Self::string(i, "AXRole") == "AXMenuItem"
                    && pane_labels.contains(&Self::string(i, "AXTitle").as_str())
            })
            .collect();
        if items.len() != 1 {
            return Err(KaxError::blocked(not_unique_reason));
        }
        self.act(&items[0], "AXPress")?;
        let until = self.deadline.min(Instant::now() + Duration::from_millis(1500));
        while !Self::pane_matches(&main, pane_labels) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        if !Self::pane_matches(&main, pane_labels) {
            return Err(KaxError::blocked(not_observed_reason));
        }
        Ok(main)
    }

    fn ensure_chats(&mut self, application: &CfRef) -> Result<CfRef> {
        self.ensure_pane(
            application,
            &["채팅", "Chats"],
            "chats_menu_unavailable",
            "chats_menu_not_unique",
            "chats_pane_not_observed",
        )
    }

    fn focus_main_for_self_menu(&mut self, application: &CfRef, main: &CfRef) -> Result<CfRef> {
        if Self::action_names(main).iter().any(|a| a == "AXRaise") {
            self.act(main, "AXRaise")?;
        }
        if Self::settable(main, "AXMain") {
            let attr = cf_string("AXMain");
            let rc = unsafe {
                AXUIElementSetAttributeValue(main.as_ptr(), attr.as_ptr(), kCFBooleanTrue)
            };
            self.actions
                .push(json!({"action": "AXMain(main_window)", "result": rc}));
        }
        let focus_deadline = self.deadline.min(Instant::now() + Duration::from_secs(1));
        while Instant::now() < focus_deadline {
            if let Some(fw) = Self::attr(application, "AXFocusedWindow")
                .or_else(|| Self::attr(application, "AXMainWindow"))
            {
                if ax_equal(&fw, main) {
                    return Ok(fw);
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Err(KaxError::blocked("main_window_focus_not_observed"))
    }

    fn observe_self_menu_window(
        &mut self,
        application: &CfRef,
        main: &CfRef,
        focused_before: &CfRef,
        before_chats: &[CfRef],
    ) -> Result<(CfRef, bool)> {
        let until = self.deadline.min(Instant::now() + Duration::from_secs(2));
        while Instant::now() < until {
            let now_chats: Vec<CfRef> = Self::elements(application, "AXWindows")
                .into_iter()
                .filter(|w| Self::string(w, "AXIdentifier") != "Main Window")
                .collect();
            let focused_after = Self::attr(application, "AXFocusedWindow")
                .or_else(|| Self::attr(application, "AXMainWindow"));
            if let Some((cand, already_open)) = resolve_self_menu_outcome(
                main,
                Some(focused_before),
                before_chats,
                &now_chats,
                focused_after.as_ref(),
                ax_equal,
            )? {
                // The self chat is always a normal chat: wait for its composer.
                if matches!(self.chat_window_shape(cand), Ok((_, true))) {
                    return Ok((cand.clone(), already_open));
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Err(KaxError::blocked("opened_window_not_observed"))
    }

    /// Fallback for hidden self-chat on macOS (`require_self = true` when not in Chats list):
    /// switches to Friends pane, locates the unique official self-profile row ("내 기본프로필"),
    /// opens its context menu, presses the unique enabled "나와의 채팅" / "Chat with myself" item,
    /// and verifies the opened/focused window's AX identity & title while rejecting homonym friends.
    fn verify_self_via_friends_menu(
        &mut self,
        application: &CfRef,
        pid: i32,
        requested_title: &str,
    ) -> Result<Map<String, Value>> {
        let before_all = Self::elements(application, "AXWindows");
        let existing_matches: Vec<CfRef> = before_all
            .iter()
            .filter(|w| {
                Self::string(w, "AXIdentifier") != "Main Window"
                    && Self::string(w, "AXTitle") == requested_title
            })
            .cloned()
            .collect();
        if existing_matches.len() > 1 {
            return Err(KaxError::blocked("target_room_window_ambiguous"));
        }
        let existing_target = existing_matches.into_iter().next();
        let before_chats: Vec<CfRef> = before_all
            .into_iter()
            .filter(|w| Self::string(w, "AXIdentifier") != "Main Window")
            .collect();

        let main = Self::main_window(application)?;
        let started_on_chats = Self::pane_matches(&main, &["채팅", "Chats"]);

        let outcome: Result<Map<String, Value>> = (|| {
            let main = self.ensure_pane(
                application,
                &["친구", "Friends"],
                "friends_menu_unavailable",
                "friends_menu_not_unique",
                "friends_pane_not_observed",
            )?;
            let focused_before = self.focus_main_for_self_menu(application, &main)?;
            // KakaoTalk Mac 26.10.0 Friends list is an `AXOutline` (e.g. AXScrollArea _NS:93 > AXOutline _NS:8).
            let main_nodes = self.nodes(&main, false, 0)?;
            let outlines: Vec<CfRef> = main_nodes
                .iter()
                .filter(|n| Self::string(n, "AXRole") == "AXOutline")
                .cloned()
                .collect();
            let list_el = if outlines.len() == 1 {
                outlines.into_iter().next().unwrap()
            } else if outlines.is_empty() {
                let tables: Vec<CfRef> = main_nodes
                    .into_iter()
                    .filter(|n| Self::string(n, "AXRole") == "AXTable")
                    .collect();
                if tables.len() != 1 {
                    return Err(KaxError::blocked("self_room_not_verified"));
                }
                tables.into_iter().next().unwrap()
            } else {
                return Err(KaxError::blocked("self_room_not_verified"));
            };

            let rows = Self::attr(&list_el, "AXVisibleRows")
                .map(|v| cf_to_elements(v.as_ptr()))
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    Self::attr(&list_el, "AXRows")
                        .map(|v| cf_to_elements(v.as_ptr()))
                        .filter(|v| !v.is_empty())
                })
                .unwrap_or_else(|| {
                    Self::elements(&list_el, "AXChildren")
                        .into_iter()
                        .filter(|c| Self::string(c, "AXRole") == "AXRow")
                        .collect()
                });
            if rows.len() > 1000 {
                return Err(KaxError::blocked("room_list_limit"));
            }

            // Locate the unique "내 기본프로필" section header row (AXDisclosureLevel == 0),
            // then follow the bidirectional AXOutline disclosure link:
            //   header_row.AXDisclosedRows == [profile_row]
            //   && CFEqual(profile_row.AXDisclosedByRow, header_row)
            //   && profile_row.AXDisclosureLevel == 1
            //   && profile_row's unique AXCell supports AXShowMenu.
            let mut headers = Vec::new();
            for row in &rows {
                self.check()?;
                let level = Self::attr(row, "AXDisclosureLevel")
                    .and_then(|v| cf_to_i64(v.as_ptr()));
                if level != Some(0) {
                    continue;
                }
                let cells: Vec<CfRef> = Self::elements(row, "AXChildren")
                    .into_iter()
                    .filter(|c| Self::string(c, "AXRole") == "AXCell")
                    .collect();
                if cells.len() != 1 {
                    continue;
                }
                let descendants = self.nodes(&cells[0], true, 0)?;
                let is_header = descendants.iter().any(|d| {
                    let role = Self::string(d, "AXRole");
                    let desc = Self::string(d, "AXDescription");
                    if SELF_PROFILE_LABELS.contains(&desc.as_str()) {
                        return true;
                    }
                    if role == "AXStaticText" {
                        let val = Self::string(d, "AXValue");
                        return SELF_PROFILE_LABELS.contains(&val.as_str());
                    }
                    false
                });
                if is_header {
                    headers.push(row.clone());
                }
            }
            if headers.len() != 1 {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            let header_row = &headers[0];

            let disclosed = Self::elements(header_row, "AXDisclosedRows");
            if disclosed.len() != 1 {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            let profile_row = &disclosed[0];
            let profile_level = Self::attr(profile_row, "AXDisclosureLevel")
                .and_then(|v| cf_to_i64(v.as_ptr()));
            if profile_level != Some(1) {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            let disclosed_by = Self::attr(profile_row, "AXDisclosedByRow")
                .ok_or_else(|| KaxError::blocked("self_room_not_verified"))?;
            if !ax_equal(&disclosed_by, header_row) {
                return Err(KaxError::blocked("self_room_not_verified"));
            }

            let profile_cells: Vec<CfRef> = Self::elements(profile_row, "AXChildren")
                .into_iter()
                .filter(|c| Self::string(c, "AXRole") == "AXCell")
                .collect();
            if profile_cells.len() != 1 {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            let cell = &profile_cells[0];
            if !Self::action_names(cell).iter().any(|a| a == "AXShowMenu") {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            if Self::elements(cell, "AXChildren")
                .iter()
                .any(|c| Self::string(c, "AXRole") == "AXMenu")
            {
                return Err(KaxError::blocked("existing_menu_preserved"));
            }
            self.act(cell, "AXShowMenu")?;

            let mut self_items: Vec<CfRef>;
            let menu_deadline = self.deadline.min(Instant::now() + Duration::from_secs(1));
            loop {
                let menus: Vec<CfRef> = Self::elements(cell, "AXChildren")
                    .into_iter()
                    .filter(|c| Self::string(c, "AXRole") == "AXMenu")
                    .collect();
                self_items = menus
                    .iter()
                    .flat_map(|m| Self::elements(m, "AXChildren"))
                    .filter(|item| {
                        Self::string(item, "AXRole") == "AXMenuItem"
                            && SELF_MENU_TITLES.contains(&Self::string(item, "AXTitle").as_str())
                            && Self::attr(item, "AXEnabled")
                                .and_then(|v| cf_to_bool(v.as_ptr()))
                                == Some(true)
                    })
                    .collect();
                if self_items.len() == 1 || Instant::now() >= menu_deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            if self_items.len() != 1 {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            self.act(&self_items[0], "AXPress")?;

            let (candidate, already_open) =
                self.observe_self_menu_window(application, &main, &focused_before, &before_chats)?;
            let actual_title = Self::string(&candidate, "AXTitle");
            if actual_title.is_empty() {
                return Err(KaxError::blocked("self_room_not_verified"));
            }
            let same_title_cnt = Self::elements(application, "AXWindows")
                .into_iter()
                .filter(|w| Self::string(w, "AXTitle") == actual_title)
                .count();
            if same_title_cnt != 1 {
                return Err(KaxError::blocked("target_room_window_ambiguous"));
            }

            // Homonym friend guard: if a window with `requested_title` was already open,
            // it MUST be the exact same AXUIElement that the official self menu focused.
            if let Some(ref exp) = existing_target {
                if !ax_equal(&candidate, exp) || actual_title != requested_title {
                    return Err(KaxError::blocked("self_room_not_verified"));
                }
            } else if actual_title != requested_title && !is_macos_self_alias(requested_title) {
                return Err(KaxError::blocked("self_room_not_verified"));
            }

            let token = register_verified_self_window(pid, &actual_title, candidate);
            let mut m = Map::new();
            m.insert("status".to_string(), json!("ok"));
            m.insert("windowTitle".to_string(), json!(actual_title));
            m.insert("alreadyOpen".to_string(), json!(already_open));
            m.insert("isSelf".to_string(), json!(true));
            m.insert("verifiedVia".to_string(), json!("friends_self_menu"));
            m.insert("windowIdentity".to_string(), json!(token));
            Ok(m)
        })();

        if started_on_chats {
            let _ = self.ensure_chats(application);
        }
        outcome
    }

    fn room_table(&mut self, main: &CfRef) -> Result<CfRef> {
        let mut tables: Vec<CfRef> = self
            .nodes(main, false, 0)?
            .into_iter()
            .filter(|n| Self::string(n, "AXRole") == "AXTable")
            .collect();
        if tables.len() != 1 {
            return Err(KaxError::blocked("room_list_not_unique"));
        }
        Ok(tables.remove(0))
    }

    fn room_entries(
        &mut self,
        main: &CfRef,
        names_only: bool,
    ) -> Result<(Vec<(CfRef, Value)>, usize, &'static str)> {
        let table = &self.room_table(main)?;
        let visible = Self::attr(table, "AXVisibleRows").map(|v| cf_to_elements(v.as_ptr()));
        let (rows, source) = match visible {
            Some(v) => (v, "AXVisibleRows"),
            None => (
                Self::elements(table, "AXChildren")
                    .into_iter()
                    .filter(|c| Self::string(c, "AXRole") == "AXRow")
                    .collect(),
                "AXChildren",
            ),
        };
        let reported_u = Self::reported_row_count(table).unwrap_or(rows.len());
        if rows.len() > 1000 {
            return Err(KaxError::blocked("room_list_limit"));
        }

        let mut result = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if let Some(entry) = self.room_entry(row, index, names_only)? {
                result.push(entry);
            }
        }
        Ok((result, reported_u, source))
    }

    /// One room-list row as `(cell, info)`, or `None` for rows that are not a single named cell.
    fn room_entry(
        &mut self,
        row: &CfRef,
        index: usize,
        names_only: bool,
    ) -> Result<Option<(CfRef, Value)>> {
        self.check()?;
        let cells: Vec<CfRef> = Self::elements(row, "AXChildren")
            .into_iter()
            .filter(|c| Self::string(c, "AXRole") == "AXCell")
            .collect();
        if cells.len() != 1 {
            return Ok(None);
        }
        let cell = &cells[0];
        let label_elements: Vec<CfRef> = Self::elements(cell, "AXChildren")
            .into_iter()
            .filter(|c| Self::string(c, "AXRole") == "AXStaticText")
            .collect();
        let Some(name_el) = label_elements.first() else {
            return Ok(None);
        };
        let name = Self::string(name_el, "AXValue");
        if name.is_empty() {
            return Ok(None);
        }
        if names_only {
            return Ok(Some((cell.clone(), json!({"name": name}))));
        }
        let labels: Vec<String> = label_elements
            .iter()
            .map(|l| Self::string(l, "AXValue"))
            .filter(|s| !s.is_empty())
            .collect();
        let descendants = self.nodes(cell, true, 0)?;
        let preview = descendants
            .iter()
            .find(|d| Self::string(d, "AXRole") == "AXTextArea")
            .map(|d| Self::string(d, "AXValue"));
        let badge = descendants.iter().any(|d| {
            Self::string(d, "AXRole") == "AXImage" && Self::string(d, "AXDescription") == ROOM_BADGE_SELF
        });
        let row_index = Self::row_index(row).unwrap_or(index as i64);
        let facts = parse_room_row_labels(&labels);
        let own = room_row_is_self(&name, badge, facts.member_count);
        let (room_type, window_title) = split_room_type_prefix(&name);
        Ok(Some((
            cell.clone(),
            json!({
                "name": name,
                "windowTitle": window_title,
                "roomType": room_type,
                "isSelf": own,
                "rowIndex": row_index,
                "labels": labels,
                "unread": facts.unread,
                "memberCount": facts.member_count,
                "lastMessageTime": facts.last_message_time,
                "muted": facts.muted,
                "pinned": facts.pinned,
                "preview": preview,
            }),
        )))
    }

    fn row_index(row: &CfRef) -> Option<i64> {
        Self::attr(row, "AXIndex").and_then(|v| cf_to_i64(v.as_ptr()))
    }

    fn scroll_value(bar: &CfRef) -> Option<f64> {
        Self::attr(bar, "AXValue").and_then(|v| cf_to_f64(v.as_ptr()))
    }

    fn set_scroll_value(bar: &CfRef, value: f64) -> bool {
        let num = cf_number_f64(value);
        let attr = cf_string("AXValue");
        let rc = unsafe { AXUIElementSetAttributeValue(bar.as_ptr(), attr.as_ptr(), num.as_ptr()) };
        rc == K_AX_ERROR_SUCCESS
    }

    /// The room list's vertical scroll bar, when the list is in a scroll area that has one.
    fn room_scroll_bar(table: &CfRef) -> Option<CfRef> {
        let mut node = Self::attr(table, "AXParent");
        for _ in 0..8 {
            let current = node?;
            if Self::string(&current, "AXRole") == "AXScrollArea" {
                return Self::attr(&current, "AXVerticalScrollBar");
            }
            node = Self::attr(&current, "AXParent");
        }
        None
    }

    /// Reads the whole room list by moving its scroll bar a page at a time from the top and
    /// collecting each page's visible rows (cheap, unlike off-screen `AXRows` children), then
    /// puts the scroll bar back where it was unless the user moved it meanwhile.
    ///
    /// Rows are told apart by `AXIndex`. A call that runs out of time returns what it has with
    /// `stopReason="tool_call_time_limit"`; the scroll position is restored in every case.
    fn room_entries_scrolled(&mut self, main: &CfRef, limit: usize) -> Result<Map<String, Value>> {
        let table = self.room_table(main)?;
        let visible_rows =
            |t: &CfRef| Self::attr(t, "AXVisibleRows").map(|v| cf_to_elements(v.as_ptr())).unwrap_or_default();
        let total_before = Self::reported_row_count(&table)
            .ok_or_else(|| KaxError::blocked("room_list_row_count_unavailable"))?;
        let bar = Self::room_scroll_bar(&table);
        let original = bar.as_ref().and_then(Self::scroll_value);
        let scrollable = match (&bar, original) {
            (Some(b), Some(_)) => Self::settable(b, "AXValue"),
            _ => false,
        };
        let hard_deadline = call_deadline().unwrap_or_else(|| self.started + call_timeout());

        let mut entries: std::collections::BTreeMap<i64, Value> = std::collections::BTreeMap::new();
        let mut steps = 0usize;
        let mut last_set: Option<f64> = None;
        let mut first_top: Option<String> = None;
        // A stop reason, or an error when nothing could be collected. Failures after some rooms
        // were read end the scroll with what was collected (the reason becomes `stopReason`).
        let outcome: Result<String> = (|| {
            let mut value = 0.0_f64;
            let mut idle = 0usize;
            let stop_or_fail = |reason: String, collected: usize| -> Result<String> {
                if collected > 0 {
                    Ok(reason)
                } else {
                    Err(KaxError::blocked(reason))
                }
            };
            loop {
                if Instant::now() >= hard_deadline {
                    return Ok("tool_call_time_limit".to_string());
                }
                // Per-page limits: stall time and AX node visits restart each page, so
                // `limit=1000` is not cut short by the 20,000-visit cap of one control call.
                self.deadline = hard_deadline.min(Instant::now() + read_timeout());
                self.visits = 0;
                let first_page = steps == 0;
                if scrollable {
                    let bar = bar.as_ref().expect("scrollable implies a bar");
                    if let (Some(prev), Some(cur)) = (last_set, Self::scroll_value(bar)) {
                        if (cur - prev).abs() > 0.002 {
                            return Ok("user_scrolled".to_string());
                        }
                    }
                    if !Self::set_scroll_value(bar, value) {
                        return stop_or_fail("room_list_scroll_failed".to_string(), entries.len());
                    }
                    steps += 1;
                }
                // Wait until the list shows the new position. On the first page the list may
                // still show the rows of where it was before (e.g. mid-list), so wait for the top
                // row itself; later pages wait (≤300 ms) for a row not read yet (the last page
                // may have none).
                let wait_ms = if first_page { 1000 } else { 300 };
                let wait_until = self.deadline.min(Instant::now() + Duration::from_millis(wait_ms));
                let shows_new_position = |rows: &[CfRef]| {
                    if first_page {
                        rows.iter().any(|r| Self::row_index(r) == Some(0))
                    } else {
                        rows.iter().any(|r| Self::row_index(r).is_some_and(|i| !entries.contains_key(&i)))
                    }
                };
                let mut rows = visible_rows(&table);
                while scrollable && Instant::now() < wait_until && !shows_new_position(&rows) {
                    std::thread::sleep(Duration::from_millis(20));
                    rows = visible_rows(&table);
                }
                if scrollable && first_page && !shows_new_position(&rows) {
                    return Err(KaxError::blocked("room_list_scroll_not_observed"));
                }
                if let Some(bar) = bar.as_ref().filter(|_| scrollable) {
                    // Remember the value the list actually settled on (it may snap), so a later
                    // difference means the user scrolled, not our own rounding.
                    last_set = Self::scroll_value(bar).or(Some(value));
                }
                let mut added = 0usize;
                for (pos, row) in rows.iter().enumerate() {
                    let Some(index) = Self::row_index(row) else { continue };
                    if entries.contains_key(&index) {
                        continue;
                    }
                    match self.room_entry(row, pos, false) {
                        Ok(Some((_, info))) => {
                            if index == 0 && first_top.is_none() {
                                first_top =
                                    info.get("name").and_then(|v| v.as_str()).map(str::to_string);
                            }
                            entries.insert(index, info);
                            added += 1;
                        }
                        Ok(None) => {}
                        Err(KaxError::Blocked(reason)) => return stop_or_fail(reason, entries.len()),
                        Err(e) => return Err(e),
                    }
                }
                idle = if added == 0 { idle + 1 } else { 0 };
                let at_end = !scrollable || value >= 1.0;
                if let Some(reason) = room_scroll_stop(entries.len(), total_before, limit, at_end, idle) {
                    return Ok(reason.to_string());
                }
                value = (value + room_scroll_step(rows.len(), total_before)).min(1.0);
            }
        })();

        // Check the list did not change under us (new message reordering it), then restore.
        let mut list_changed = Self::reported_row_count(&table) != Some(total_before);
        let mut restored = !scrollable;
        let mut user_scrolled = false;
        if let (true, Some(bar), Some(orig), Some(last)) = (scrollable, bar.as_ref(), original, last_set) {
            match Self::scroll_value(bar) {
                Some(cur) if (cur - last).abs() <= 0.002 => {
                    if first_top.is_some() && Self::set_scroll_value(bar, 0.0) {
                        std::thread::sleep(Duration::from_millis(100));
                        let top = visible_rows(&table)
                            .into_iter()
                            .find(|r| Self::row_index(r) == Some(0))
                            .and_then(|r| self.room_entry(&r, 0, true).ok().flatten())
                            .and_then(|(_, info)| info.get("name").and_then(|v| v.as_str()).map(str::to_string));
                        if top.is_some() && top != first_top {
                            list_changed = true;
                        }
                    }
                    restored = Self::set_scroll_value(bar, orig);
                }
                _ => user_scrolled = true,
            }
            self.actions.push(json!({
                "action": "AXValue(roomListScroll)",
                "steps": steps,
                "restored": restored,
            }));
        }
        let stop_reason = outcome?;
        if stop_reason == "user_scrolled" {
            user_scrolled = true;
        }

        let mut items: Vec<Value> = entries.into_values().collect();
        items.truncate(limit);
        let complete = items.len() >= total_before && !list_changed;
        let mut m = Map::new();
        m.insert("status".to_string(), json!("ok"));
        m.insert("scope".to_string(), json!("scrolled_room_list"));
        m.insert("reportedRowCount".to_string(), json!(total_before));
        m.insert("rowsSource".to_string(), json!("AXVisibleRows+scroll"));
        m.insert("itemsCollected".to_string(), json!(items.len()));
        m.insert("items".to_string(), Value::Array(items));
        m.insert("historyComplete".to_string(), json!(complete));
        m.insert("stopReason".to_string(), json!(stop_reason));
        m.insert("scrollSteps".to_string(), json!(steps));
        m.insert("scrollRestored".to_string(), json!(restored));
        m.insert("userScrolled".to_string(), json!(user_scrolled));
        m.insert("listChanged".to_string(), json!(list_changed));
        Ok(m)
    }

    /// Closes the exact chat window titled `title`. A composer with text blocks the close
    /// (`existing_draft_preserved`). A window without a composer (one-way channel window) has no
    /// draft to lose and is closed as is; several composers still block.
    fn close_window(&mut self, application: &CfRef, title: &str) -> Result<Map<String, Value>> {
        let target = Self::titled_window(application, title)?;
        if !Self::elements(&target, "AXSheets").is_empty() {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        let nodes = self.nodes(&target, false, 0)?;
        let (composers, other_text_areas): (Vec<&CfRef>, Vec<&CfRef>) = nodes
            .iter()
            .filter(|n| Self::string(n, "AXRole") == "AXTextArea")
            .partition(|n| COMPOSER_LABELS.contains(&Self::string(n, "AXDescription").as_str()));
        if !close_allowed_composers(composers.len()) {
            return Err(KaxError::blocked("chat_composer_not_unique"));
        }
        if let Some(composer) = composers.first() {
            if !Self::composer_text(composer)?.is_empty() {
                return Err(KaxError::blocked("existing_draft_preserved"));
            }
        } else {
            // No recognised composer: close only a one-way channel window (exactly one
            // transcript table), never some other window that happens to share the title, and
            // never one with text in an unrecognised text area (a composer whose label changed).
            let tables = nodes.iter().filter(|n| Self::string(n, "AXRole") == "AXTable").count();
            chat_window_check(0, tables).map_err(KaxError::blocked)?;
            for area in other_text_areas {
                if !Self::composer_text(area)?.is_empty() {
                    return Err(KaxError::blocked("existing_draft_preserved"));
                }
            }
        }
        let close = Self::attr(&target, "AXCloseButton")
            .ok_or_else(|| KaxError::blocked("close_button_unavailable"))?;
        self.act(&close, "AXPress")?;
        let until = self.deadline.min(Instant::now() + Duration::from_millis(1500));
        while Instant::now() < until {
            if !Self::elements(application, "AXWindows")
                .iter()
                .any(|w| Self::string(w, "AXTitle") == title)
            {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("ok"));
                m.insert("windowTitle".to_string(), json!(title));
                m.insert("alreadyClosed".to_string(), json!(false));
                m.insert("hadComposer".to_string(), json!(!composers.is_empty()));
                return Ok(m);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(KaxError::blocked("closed_window_not_observed"))
    }

    /// The current room list as a `rooms` result: visible rows, or the whole list with
    /// `scroll=true`.
    fn rooms_report(&mut self, main: &CfRef, args: &Map<String, Value>) -> Result<Map<String, Value>> {
        let search_query = self.room_search_query(main)?;
        // Exactly one field is the search bar; several leave the state unknown (null).
        let search_open = match self.room_search_fields(main)?.len() {
            0 => json!(false),
            1 => json!(true),
            _ => Value::Null,
        };
        if args.get("scroll") == Some(&Value::Bool(true)) {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .filter(|l| (1..=MAX_ROOM_SCROLL_LIMIT as u64).contains(l))
                .ok_or_else(|| KaxError::blocked("invalid_control_request"))?;
            let mut m = self.room_entries_scrolled(main, limit as usize)?;
            m.insert("searchQuery".to_string(), json!(search_query));
            m.insert("searchOpen".to_string(), json!(search_open));
            return Ok(m);
        }
        let (entries, reported, rows_source) = self.room_entries(main, false)?;
        let mut m = Map::new();
        m.insert("status".to_string(), json!("ok"));
        let items: Vec<Value> = entries.into_iter().map(|(_, info)| info).collect();
        m.insert("visibleItemsReturned".to_string(), json!(items.len()));
        m.insert("items".to_string(), Value::Array(items));
        m.insert("scope".to_string(), json!("currently_visible_room_list"));
        m.insert("reportedRowCount".to_string(), json!(reported));
        m.insert("rowsSource".to_string(), json!(rows_source));
        m.insert("historyComplete".to_string(), json!(false));
        m.insert("searchQuery".to_string(), json!(search_query));
        m.insert("searchOpen".to_string(), json!(search_open));
        Ok(m)
    }

    /// After a room-search change, waits for the room table to show the new results. KakaoTalk
    /// debounces the search and may flash an intermediate list first, so this waits at least
    /// 650 ms (the delay the old fixed sleep used), then until the table differs from `before`
    /// and has stayed the same for 300 ms. Capped at 3 s; identical results are possible, so
    /// running out of time is not an error.
    fn wait_room_table_reload(&mut self, table: &CfRef, before: &(Option<usize>, Vec<String>)) {
        let started = Instant::now();
        let until = started + Duration::from_millis(3000);
        let min_wait = Duration::from_millis(650);
        let quiet = Duration::from_millis(300);
        let mut current = self.room_table_signature(table);
        let mut stable_since = Instant::now();
        loop {
            let now = Instant::now();
            if now >= until {
                return;
            }
            if now.duration_since(started) >= min_wait
                && current != *before
                && now.duration_since(stable_since) >= quiet
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(60));
            let next = self.room_table_signature(table);
            if next != current {
                current = next;
                stable_since = Instant::now();
            }
        }
    }

    /// Closes the main window's room search bar with its header "검색" toggle button (the field
    /// has no AXCancel); closing also clears the query and shows the whole list again. No-op
    /// when the bar is already closed.
    fn close_room_search(&mut self, main: &CfRef) -> Result<()> {
        let fields = self.room_search_fields(main)?;
        let field = match fields.as_slice() {
            [] => return Ok(()),
            [f] => f.clone(),
            // Another text field in the pane: the toggle state is unclear, press nothing.
            _ => return Err(KaxError::blocked("search_field_not_unique")),
        };
        // The scan stops at text fields, so this is the header toggle, not the icon inside the
        // field.
        let buttons: Vec<CfRef> = self
            .nodes(main, false, 0)?
            .into_iter()
            .filter(|n| {
                Self::string(n, "AXRole") == "AXButton"
                    && matches!(Self::string(n, "AXDescription").as_str(), "검색" | "Search")
            })
            .collect();
        if buttons.len() != 1 {
            return Err(KaxError::blocked("search_button_not_unique"));
        }
        self.act(&buttons[0], "AXPress")?;
        let until = self.deadline.min(Instant::now() + Duration::from_millis(1500));
        while self.room_search_fields(main)?.iter().any(|f| ax_equal(f, &field)) {
            if Instant::now() >= until {
                return Err(KaxError::blocked("room_search_close_not_observed"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// The room-list search text, or "" when the search field is closed or ambiguous.
    fn room_search_query(&mut self, main: &CfRef) -> Result<String> {
        let fields = self.room_search_fields(main)?;
        Ok(if fields.len() == 1 { Self::string(&fields[0], "AXValue") } else { String::new() })
    }

    fn room_search_fields(&mut self, main: &CfRef) -> Result<Vec<CfRef>> {
        Ok(self
            .nodes(main, false, 0)?
            .into_iter()
            .filter(|n| matches!(Self::string(n, "AXRole").as_str(), "AXTextField" | "AXSearchField"))
            .collect())
    }

    /// Folders are hidden while a room search is typed in (the list shows search results).
    fn ensure_no_room_search(&mut self, main: &CfRef) -> Result<()> {
        let fields = self.room_search_fields(main)?;
        if fields.len() > 1 {
            return Err(KaxError::blocked("search_field_not_unique"));
        }
        if fields.first().is_some_and(|f| !Self::string(f, "AXValue").is_empty()) {
            return Err(KaxError::blocked("room_search_active"));
        }
        Ok(())
    }

    /// The folder bar above the room list: `(list, folders, selected folder index)`.
    fn room_folders(&mut self, main: &CfRef) -> Result<(CfRef, Vec<RoomFolder>, Option<usize>)> {
        let lists: Vec<CfRef> = self
            .nodes(main, false, 0)?
            .into_iter()
            .filter(|n| {
                Self::string(n, "AXRole") == "AXList" && Self::string(n, "AXTitle") == ROOM_FOLDER_LIST_TITLE
            })
            .collect();
        if lists.len() != 1 {
            return Err(KaxError::blocked("room_folders_unavailable"));
        }
        let list = lists.into_iter().next().unwrap();
        // The collection holds one section whose children are the folder tabs.
        let mut groups = Vec::new();
        for child in Self::elements(&list, "AXChildren") {
            if Self::string(&child, "AXRole") == "AXList" {
                groups.extend(Self::elements(&child, "AXChildren"));
            } else {
                groups.push(child);
            }
        }
        let mut folders = Vec::new();
        for group in groups {
            let label = Self::elements(&group, "AXChildren")
                .into_iter()
                .find(|c| Self::string(c, "AXRole") == "AXStaticText")
                .map(|t| Self::string(&t, "AXValue"));
            if let Some((name, unread)) = label.as_deref().and_then(parse_folder_label) {
                folders.push(RoomFolder { group, name, unread });
            }
        }
        if folders.is_empty() {
            return Err(KaxError::blocked("room_folders_unavailable"));
        }
        let selected = Self::attr(&list, "AXSelectedChildren")
            .map(|v| cf_to_elements(v.as_ptr()))
            .and_then(|sel| {
                let first = sel.first()?.clone();
                folders.iter().position(|f| ax_equal(&f.group, &first))
            });
        Ok((list, folders, selected))
    }

    fn folders_report(&mut self, main: &CfRef) -> Result<Map<String, Value>> {
        self.ensure_no_room_search(main)?;
        let (_, folders, selected) = self.room_folders(main)?;
        let items: Vec<Value> = folders
            .iter()
            .enumerate()
            .map(|(i, f)| json!({"name": f.name, "index": i, "unread": f.unread, "selected": selected == Some(i)}))
            .collect();
        let mut m = Map::new();
        m.insert("status".to_string(), json!("ok"));
        m.insert("items".to_string(), Value::Array(items));
        m.insert("selected".to_string(), json!(selected.map(|i| folders[i].name.clone())));
        Ok(m)
    }

    /// What the room table shows: its row count and the names on the visible rows. Used to
    /// tell that the table really reloaded after a folder switch.
    fn room_table_signature(&mut self, table: &CfRef) -> (Option<usize>, Vec<String>) {
        let rows = Self::attr(table, "AXVisibleRows").map(|v| cf_to_elements(v.as_ptr())).unwrap_or_default();
        let names = rows
            .iter()
            .take(8)
            .enumerate()
            .filter_map(|(i, r)| self.room_entry(r, i, true).ok().flatten())
            .filter_map(|(_, info)| info.get("name").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        (Self::reported_row_count(table), names)
    }

    /// Selects folder `group` in the folder bar (`AXSelectedChildren`, no mouse), then waits
    /// until the selection shows it and the room table has visibly reloaded: its row count or
    /// visible names must differ from `before` and then stay the same for a moment. A switch
    /// whose reload is never seen is an error (`room_folder_reload_not_observed`), so a read can
    /// never report the previous folder's rooms under the new folder's name.
    fn select_room_folder(
        &mut self,
        list: &CfRef,
        group: &CfRef,
        table: &CfRef,
        before: &(Option<usize>, Vec<String>),
    ) -> Result<()> {
        let values = [group.as_ptr()];
        let arr = CfRef(unsafe { CFArrayCreate(std::ptr::null(), values.as_ptr(), 1, std::ptr::null()) });
        let attr = cf_string("AXSelectedChildren");
        let rc = unsafe { AXUIElementSetAttributeValue(list.as_ptr(), attr.as_ptr(), arr.as_ptr()) };
        self.actions.push(json!({"action": "AXSelectedChildren(roomFolder)", "result": rc}));
        if rc != K_AX_ERROR_SUCCESS {
            return Err(KaxError::blocked("room_folder_select_failed"));
        }
        let until = self.deadline.min(Instant::now() + Duration::from_millis(2000));
        let shows_group = || {
            Self::attr(list, "AXSelectedChildren")
                .map(|v| cf_to_elements(v.as_ptr()))
                .is_some_and(|sel| sel.len() == 1 && ax_equal(&sel[0], group))
        };
        while !shows_group() {
            if Instant::now() >= until {
                return Err(KaxError::blocked("room_folder_select_not_observed"));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let mut current = self.room_table_signature(table);
        while current == *before {
            if Instant::now() >= until {
                return Err(KaxError::blocked("room_folder_reload_not_observed"));
            }
            std::thread::sleep(Duration::from_millis(40));
            current = self.room_table_signature(table);
        }
        // Reloaded: wait until two readings 100 ms apart agree.
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let next = self.room_table_signature(table);
            if next == current {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(KaxError::blocked("room_folder_reload_not_observed"));
            }
            current = next;
        }
    }

    /// `rooms` for one folder: switches the folder bar to it, reads the room list like `rooms`
    /// (visible rows or `scroll=true`), and switches back to the folder that was selected
    /// before. Once a switch was attempted, every outcome (read errors, a switch that was not
    /// observed) goes through the restore, and its result is reported (`folderRestored`), also
    /// in blocked results.
    fn rooms_in_folder(
        &mut self,
        main: &CfRef,
        folder: &str,
        args: &Map<String, Value>,
    ) -> Result<Map<String, Value>> {
        self.ensure_no_room_search(main)?;
        let (list, folders, original) = self.room_folders(main)?;
        let matching: Vec<usize> = folders
            .iter()
            .enumerate()
            .filter(|(_, f)| f.name == folder)
            .map(|(i, _)| i)
            .collect();
        let target = match matching.as_slice() {
            [] => return Err(KaxError::blocked("folder_not_found")),
            [i] => *i,
            _ => return Err(KaxError::blocked("room_folder_name_ambiguous")),
        };
        // Without knowing the current folder it could not be restored: change nothing.
        let Some(original) = original else {
            return Err(KaxError::blocked("room_folder_selection_unknown"));
        };
        let table = self.room_table(main)?;
        let changed = original != target;
        // Switching folders resets the shared table's scroll position; remember it.
        let original_scroll = Self::room_scroll_bar(&table).and_then(|bar| Self::scroll_value(&bar));
        let outcome = if changed {
            let before = self.room_table_signature(&table);
            self.select_room_folder(&list, &folders[target].group, &table, &before)
                .and_then(|()| self.rooms_report(main, args))
        } else {
            self.rooms_report(main, args)
        };
        let restored = if changed {
            // The restore always gets its own 3 s, even when the read used up the stall deadline
            // or the call budget: leaving the user in another folder is worse than overrunning.
            self.deadline = Instant::now() + Duration::from_secs(3);
            let before = self.room_table_signature(&table);
            let back = self.select_room_folder(&list, &folders[original].group, &table, &before);
            // The table may legitimately look the same if the read failed before the switch
            // showed; the selection itself is what must be back.
            let selected_back = Self::attr(&list, "AXSelectedChildren")
                .map(|v| cf_to_elements(v.as_ptr()))
                .is_some_and(|sel| sel.len() == 1 && ax_equal(&sel[0], &folders[original].group));
            let is_back = back.is_ok() || selected_back;
            if let (true, Some(value)) = (is_back, original_scroll) {
                if let Some(bar) = Self::room_scroll_bar(&table) {
                    if Self::set_scroll_value(&bar, value) {
                        self.actions.push(json!({"action": "AXValue(roomListScrollRestore)", "result": 0}));
                    }
                }
            }
            json!(is_back)
        } else {
            Value::Null
        };
        let folder_info =
            json!({"name": folders[target].name, "index": target, "unread": folders[target].unread});
        let mut m = match outcome {
            Ok(m) => m,
            Err(KaxError::Blocked(reason)) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m
            }
            Err(e) => return Err(e),
        };
        m.insert("folder".to_string(), folder_info);
        m.insert("folderChanged".to_string(), json!(changed));
        m.insert("folderRestored".to_string(), restored);
        Ok(m)
    }

    fn popover_of(button: &CfRef) -> Option<CfRef> {
        Self::elements(button, "AXChildren")
            .into_iter()
            .find(|c| Self::string(c, "AXRole") == "AXPopover")
    }

    /// Reads a group room's member list: presses the window's "대화상대 N명" button, reads the
    /// popover's table rows (off-screen rows are cheap enough here, ~15 ms each, so no
    /// scrolling), then closes the popover with `AXCancel`. A popover the user already had
    /// open is read and left open. The popover's search field is never touched.
    fn read_members(&mut self, window: &CfRef, limit: usize) -> Result<Map<String, Value>> {
        let buttons: Vec<(CfRef, u64)> = Self::elements(window, "AXChildren")
            .into_iter()
            .filter(|c| Self::string(c, "AXRole") == "AXButton")
            .filter_map(|c| {
                parse_member_button(&Self::string(&c, "AXTitle"), &Self::string(&c, "AXDescription"))
                    .map(|n| (c, n))
            })
            .collect();
        let (button, member_count) = match buttons.len() {
            0 => return Err(KaxError::blocked("member_list_unavailable")),
            1 => buttons.into_iter().next().unwrap(),
            _ => return Err(KaxError::blocked("member_button_not_unique")),
        };

        let already_open = Self::popover_of(&button);
        let popover = match already_open.clone() {
            Some(p) => p,
            // KakaoTalk ignores the press for ~0.5 s after the list was closed, so a press that
            // shows nothing within 700 ms is repeated (normally the list appears in 0.1–0.25 s).
            // The button toggles the list, so before every re-press check whether a slow earlier
            // press has opened it meanwhile, and give the last press extra time to show up.
            None => {
                let mut found = None;
                for attempt in 0..3 {
                    if let Some(p) = Self::popover_of(&button) {
                        found = Some(p);
                        break;
                    }
                    self.act(&button, "AXPress")?;
                    let wait = if attempt == 2 { 1500 } else { 700 };
                    let until = self.deadline.min(Instant::now() + Duration::from_millis(wait));
                    while found.is_none() && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(30));
                        found = Self::popover_of(&button);
                    }
                    if found.is_some() {
                        break;
                    }
                }
                found.ok_or_else(|| KaxError::blocked("member_list_not_observed"))?
            }
        };

        let hard_deadline = call_deadline().unwrap_or_else(|| self.started + call_timeout());
        let mut items = Vec::new();
        let mut table_rows = 0usize;
        let mut skipped_rows = 0usize;
        let outcome: Result<&'static str> = (|| {
            let tables: Vec<CfRef> = self
                .nodes(&popover, false, 0)?
                .into_iter()
                .filter(|n| Self::string(n, "AXRole") == "AXTable")
                .collect();
            if tables.len() != 1 {
                return Err(KaxError::blocked("member_table_not_unique"));
            }
            // A failed rows read must not pass as an empty member list.
            let rows = Self::attr(&tables[0], "AXRows")
                .map(|v| cf_to_elements(v.as_ptr()))
                .ok_or_else(|| KaxError::blocked("member_rows_unavailable"))?;
            table_rows = rows.len();
            let mut row_started = Instant::now();
            for (pos, row) in rows.iter().enumerate() {
                let now = Instant::now();
                if now >= hard_deadline {
                    return Ok("tool_call_time_limit");
                }
                // Stall limit per row, as in transcript reads: one row taking longer than the
                // read timeout ends the read with what was collected.
                if now.duration_since(row_started) >= read_timeout() {
                    return Ok("member_row_stalled");
                }
                row_started = now;
                // The table ends with an empty spacer row; any other row that cannot be read as
                // a named member is counted instead of silently dropped.
                let is_last = pos + 1 == rows.len();
                let cells: Vec<CfRef> = Self::elements(row, "AXChildren")
                    .into_iter()
                    .filter(|c| Self::string(c, "AXRole") == "AXCell")
                    .collect();
                if cells.len() != 1 {
                    if !is_last {
                        skipped_rows += 1;
                    }
                    continue;
                }
                let mut name = String::new();
                let mut badges = Vec::new();
                for child in Self::elements(&cells[0], "AXChildren") {
                    match Self::string(&child, "AXRole").as_str() {
                        "AXStaticText" if name.is_empty() => name = Self::string(&child, "AXValue"),
                        "AXImage" => {
                            let desc = Self::string(&child, "AXDescription");
                            if !desc.is_empty() {
                                badges.push(desc);
                            }
                        }
                        _ => {}
                    }
                }
                if name.is_empty() {
                    if !is_last {
                        skipped_rows += 1;
                    }
                    continue;
                }
                let row_index = Self::row_index(row).unwrap_or(pos as i64);
                items.push(member_item(&name, row_index, &badges));
                if items.len() >= limit {
                    // Done unless named rows remain (only the trailing spacer may be left).
                    return Ok(member_limit_stop(pos, rows.len()));
                }
            }
            Ok("complete")
        })();

        let mut closed = false;
        if already_open.is_none() {
            let cancel = cf_string("AXCancel");
            let rc = unsafe { AXUIElementPerformAction(popover.as_ptr(), cancel.as_ptr()) };
            self.actions.push(json!({"action": "AXCancel(memberList)", "result": rc}));
            let until = Instant::now() + Duration::from_secs(1);
            while Instant::now() < until {
                if Self::popover_of(&button).is_none() {
                    closed = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        let stop_reason = outcome?;

        let complete =
            stop_reason == "complete" && skipped_rows == 0 && items.len() as u64 == member_count;
        let mut m = Map::new();
        m.insert("status".to_string(), json!("ok"));
        m.insert("memberCount".to_string(), json!(member_count));
        m.insert("tableRowCount".to_string(), json!(table_rows));
        m.insert("itemsCollected".to_string(), json!(items.len()));
        m.insert("skippedRows".to_string(), json!(skipped_rows));
        m.insert("items".to_string(), Value::Array(items));
        m.insert("complete".to_string(), json!(complete));
        m.insert("stopReason".to_string(), json!(stop_reason));
        m.insert("listWasOpen".to_string(), json!(already_open.is_some()));
        m.insert("listClosed".to_string(), json!(closed));
        Ok(m)
    }

    fn search_field(&mut self, container: &CfRef, open: bool) -> Result<CfRef> {
        let mut found: Vec<CfRef> = self
            .nodes(container, false, 0)?
            .into_iter()
            .filter(|n| matches!(Self::string(n, "AXRole").as_str(), "AXTextField" | "AXSearchField"))
            .collect();
        if found.is_empty() && open {
            let buttons: Vec<CfRef> = self
                .nodes(container, false, 0)?
                .into_iter()
                .filter(|n| {
                    Self::string(n, "AXRole") == "AXButton"
                        && matches!(Self::string(n, "AXDescription").as_str(), "검색" | "Search")
                })
                .collect();
            if buttons.len() != 1 {
                return Err(KaxError::blocked("search_button_not_unique"));
            }
            self.act(&buttons[0], "AXPress")?;
            let until = self.deadline.min(Instant::now() + Duration::from_secs(1));
            while found.is_empty() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
                found = self
                    .nodes(container, false, 0)?
                    .into_iter()
                    .filter(|n| {
                        matches!(
                            Self::string(n, "AXRole").as_str(),
                            "AXTextField" | "AXSearchField"
                        )
                    })
                    .collect();
            }
        }
        if found.len() != 1 {
            return Err(KaxError::blocked("search_field_not_unique"));
        }
        Ok(found.into_iter().next().unwrap())
    }

    fn search_buttons(field: &CfRef) -> Result<Vec<CfRef>> {
        let parent = Self::attr(field, "AXParent")
            .ok_or_else(|| KaxError::blocked("search_toolbar_unavailable"))?;
        let siblings = Self::elements(&parent, "AXChildren");
        let Some(pos) = siblings.iter().position(|s| ax_equal(s, field)) else {
            return Err(KaxError::blocked("search_field_missing"));
        };
        let buttons: Vec<CfRef> = siblings
            .into_iter()
            .skip(pos + 1)
            .take(SEARCH_TOOLBAR_BUTTON_COUNT)
            .collect();
        if buttons.len() != SEARCH_TOOLBAR_BUTTON_COUNT
            || !buttons.iter().all(|b| Self::string(b, "AXRole") == "AXButton")
        {
            return Err(KaxError::blocked("search_toolbar_layout_unknown"));
        }
        let frames: Vec<Option<AxFrame>> = buttons.iter().map(Self::frame).collect();
        check_search_toolbar_geometry(Self::frame(field), &frames)?;
        Ok(buttons)
    }

    /// `AXPosition` + `AXSize` of `el`, or `None` if either is unavailable.
    fn frame(el: &CfRef) -> Option<AxFrame> {
        let pos = Self::attr(el, "AXPosition")?;
        let size = Self::attr(el, "AXSize")?;
        let axval_tid = unsafe { AXValueGetTypeID() };
        if unsafe { CFGetTypeID(pos.as_ptr()) != axval_tid || CFGetTypeID(size.as_ptr()) != axval_tid }
        {
            return None;
        }
        let mut pt = CGPoint::default();
        let mut sz = CGSize::default();
        let ok = unsafe {
            AXValueGetValue(pos.as_ptr(), K_AX_VALUE_CGPOINT_TYPE, (&mut pt as *mut CGPoint).cast()) != 0
                && AXValueGetValue(size.as_ptr(), K_AX_VALUE_CGSIZE_TYPE, (&mut sz as *mut CGSize).cast())
                    != 0
        };
        ok.then_some(AxFrame {
            x: pt.x,
            y: pt.y,
            w: sz.width,
            h: sz.height,
        })
    }

    fn search_state(&mut self, target: &CfRef, field: &CfRef, query: &str) -> Result<Value> {
        let buttons = Self::search_buttons(field)?;
        let tables: Vec<CfRef> = self
            .nodes(target, false, 0)?
            .into_iter()
            .filter(|n| Self::string(n, "AXRole") == "AXTable")
            .collect();
        if tables.len() != 1 {
            return Err(KaxError::blocked("chat_transcript_not_unique"));
        }
        let visible = Self::elements(&tables[0], "AXVisibleRows");
        let needle = caseless::default_case_fold_str(query);
        let mut rows = Vec::new();
        let mut matches = 0usize;
        for row in visible {
            let parts = self.nodes(&row, true, 0)?;
            let bodies: Vec<String> = parts
                .iter()
                .filter(|p| Self::string(p, "AXRole") == "AXTextArea")
                .map(|p| Self::string(p, "AXValue"))
                .collect();
            matches += bodies
                .iter()
                .filter(|b| caseless::default_case_fold_str(b).contains(&needle))
                .count();
            let labels: Vec<String> = parts
                .iter()
                .filter(|p| Self::string(p, "AXRole") == "AXStaticText")
                .map(|p| Self::string(p, "AXValue"))
                .collect();
            let idx = Self::attr(&row, "AXIndex")
                .and_then(|v| cf_to_i64(v.as_ptr()))
                .unwrap_or(-1);
            rows.push(json!({"index": idx, "bodies": bodies, "labels": labels}));
        }
        let prev = Self::attr(&buttons[0], "AXEnabled")
            .and_then(|v| cf_to_bool(v.as_ptr()))
            .ok_or_else(|| KaxError::blocked("search_navigation_state_unavailable"))?;
        let next = Self::attr(&buttons[1], "AXEnabled")
            .and_then(|v| cf_to_bool(v.as_ptr()))
            .ok_or_else(|| KaxError::blocked("search_navigation_state_unavailable"))?;
        Ok(json!({
            "query": Self::string(field, "AXValue"),
            "visibleRows": rows,
            "visibleMatchCount": matches,
            "previousEnabled": prev,
            "nextEnabled": next,
        }))
    }

    fn finalize_report(&self, mut report: Map<String, Value>) -> Value {
        report.insert("source".to_string(), json!("macos_accessibility"));
        report.insert("databaseCalls".to_string(), json!(0));
        report.insert("dispatchAttempted".to_string(), json!(false));
        report.insert("uiActions".to_string(), json!(self.actions.len()));
        report.insert("actions".to_string(), Value::Array(self.actions.clone()));
        report.insert(
            "elapsedMs".to_string(),
            json!(self.started.elapsed().as_millis() as u64),
        );
        Value::Object(report)
    }
}

struct VerifiedSelfWindow {
    pid: i32,
    title: String,
    window: CfRef,
}

static VERIFIED_SELF_WINDOWS: OnceLock<Mutex<HashMap<String, VerifiedSelfWindow>>> =
    OnceLock::new();
static VERIFIED_SELF_SEQ: AtomicU64 = AtomicU64::new(1);

fn register_verified_self_window(pid: i32, title: &str, window: CfRef) -> String {
    let seq = VERIFIED_SELF_SEQ.fetch_add(1, Ordering::Relaxed);
    let token = format!("macos-self-ax:{pid}:{seq}");
    let map = VERIFIED_SELF_WINDOWS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|p| p.into_inner());
    if guard.len() >= 64 {
        guard.clear();
    }
    guard.insert(
        token.clone(),
        VerifiedSelfWindow {
            pid,
            title: title.to_string(),
            window,
        },
    );
    token
}

fn lookup_verified_self_window(token: &str) -> Option<(i32, String, CfRef)> {
    let map = VERIFIED_SELF_WINDOWS.get_or_init(|| Mutex::new(HashMap::new()));
    let guard = map.lock().unwrap_or_else(|p| p.into_inner());
    guard
        .get(token)
        .map(|entry| (entry.pid, entry.title.clone(), entry.window.clone()))
}

struct SendCtx {
    started: Instant,
    deadline: Instant,
    visited: usize,
    dispatch_attempted: bool,
    composer_changed: bool,
}

impl SendCtx {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            deadline: now + Duration::from_secs(8),
            visited: 0,
            dispatch_attempted: false,
            composer_changed: false,
        }
    }

    fn descendants(&mut self, el: &CfRef, depth: usize) -> Result<Vec<CfRef>> {
        self.visited += 1;
        if self.visited > 600 || depth > 10 || Instant::now() >= self.deadline {
            return Err(KaxError::blocked("control_tree_limit"));
        }
        let role = ControlCtx::string(el, "AXRole");
        if matches!(role.as_str(), "AXTable" | "AXRow" | "AXCell" | "AXPopover") {
            return Ok(Vec::new());
        }
        if matches!(role.as_str(), "AXSheet" | "AXDialog") {
            return Err(KaxError::blocked("modal_dialog_present"));
        }
        let mut out = vec![el.clone()];
        for child in ControlCtx::elements(el, "AXChildren") {
            out.extend(self.descendants(&child, depth + 1)?);
        }
        Ok(out)
    }

    fn controls(&mut self, window: &CfRef) -> Result<(CfRef, CfRef)> {
        let nodes = self.descendants(window, 0)?;
        let composers: Vec<CfRef> = nodes
            .iter()
            .filter(|n| {
                ControlCtx::string(n, "AXRole") == "AXTextArea"
                    && ControlCtx::settable(n, "AXValue")
                    && COMPOSER_LABELS.contains(&ControlCtx::string(n, "AXDescription").as_str())
            })
            .cloned()
            .collect();
        let buttons: Vec<CfRef> = nodes
            .iter()
            .filter(|n| {
                ControlCtx::string(n, "AXRole") == "AXButton"
                    && SEND_BUTTON_TITLES.contains(&ControlCtx::string(n, "AXTitle").as_str())
            })
            .cloned()
            .collect();
        if composers.is_empty() {
            // One-way channel windows (알림톡 etc.) have a transcript but nothing to type into.
            return Err(KaxError::blocked("chat_composer_unavailable"));
        }
        if composers.len() != 1 || buttons.len() != 1 {
            return Err(KaxError::blocked("composer_or_send_button_not_unique"));
        }
        if !ControlCtx::action_names(&buttons[0])
            .iter()
            .any(|a| a == "AXPress")
        {
            return Err(KaxError::blocked("send_button_has_no_axpress"));
        }
        Ok((
            composers.into_iter().next().unwrap(),
            buttons.into_iter().next().unwrap(),
        ))
    }

    fn finalize_report(&self, mut report: Map<String, Value>) -> Value {
        report.insert("dispatchAttempted".to_string(), json!(self.dispatch_attempted));
        report.insert("composerChanged".to_string(), json!(self.composer_changed));
        report.insert("controlNodesVisited".to_string(), json!(self.visited));
        report.insert("conversationRead".to_string(), json!(false));
        report.insert(
            "elapsedMs".to_string(),
            json!(self.started.elapsed().as_millis() as u64),
        );
        Value::Object(report)
    }
}

// ---------------------------------------------------------------------------
// Live MacosAxDriver implementing AxDriver
// ---------------------------------------------------------------------------

pub struct MacosAxDriver;

impl MacosAxDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }

    fn make_application(timeout: f32) -> Result<(CfRef, i32)> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return Err(KaxError::blocked("accessibility_permission_required"));
        }
        let pid = find_kakaotalk_pid()?;
        let app = unsafe { CfRef::from_owned(AXUIElementCreateApplication(pid)) }
            .ok_or_else(|| KaxError::blocked("app_not_running"))?;
        unsafe {
            AXUIElementSetMessagingTimeout(app.as_ptr(), timeout);
        }
        Ok((app, pid))
    }

    /// The unique chat window titled `target_title` and its transcript tables (checked readable).
    fn target_chat_window(
        ctx: &mut ReadCtx,
        windows: Vec<CfRef>,
        target_title: &str,
    ) -> Result<(CfRef, Vec<CfRef>)> {
        let mut matches = Vec::new();
        for w in windows {
            if ctx.string(&w, "AXTitle", false)? == target_title {
                matches.push(w);
            }
        }
        if matches.len() != 1 {
            return Err(KaxError::blocked(if matches.is_empty() {
                "target_room_window_not_open"
            } else {
                "target_room_window_ambiguous"
            }));
        }
        let window = matches.into_iter().next().unwrap();
        // The main window (room list table, no composer) must never pass as a one-way chat.
        if ctx.string(&window, "AXIdentifier", true)? == "Main Window" {
            return Err(KaxError::blocked("main_window_is_not_a_chat"));
        }
        let (composers, tables) = ctx.controls(&window)?;
        readable_transcript_check(composers, tables.len()).map_err(KaxError::blocked)?;
        Ok((window, tables))
    }

    /// Reads only the end of a chat transcript for send confirmation: the rows from `start` (or
    /// the last `last` rows) to the end, with on-screen frames of the message parts, refusing more
    /// than [`SEND_TAIL_MAX_ROWS`] rows. Same stability check as a full read.
    fn run_read_tail(title: &str, start: Option<usize>, last: usize) -> Value {
        let mut ctx = ReadCtx::new();
        ctx.collect_frames = true;
        let res: Result<Map<String, Value>> = (|| {
            if title.is_empty() {
                return Err(KaxError::blocked("exact_window_title_required"));
            }
            let (application, pid) = Self::make_application(0.2)?;
            let windows = ctx.elements(&application, "AXWindows")?;
            let (window, tables) = Self::target_chat_window(&mut ctx, windows, title)?;
            let count = ControlCtx::reported_row_count(&tables[0])
                .ok_or_else(|| KaxError::blocked("tail_rows_unsupported"))?;
            let want = match start {
                Some(s) if s > count => return Err(KaxError::blocked("transcript_rows_shrank")),
                Some(s) => count - s,
                None => last.min(count),
            };
            if want > SEND_TAIL_MAX_ROWS {
                return Err(KaxError::blocked("send_tail_rows_exceeded"));
            }
            let before = ctx.rows(&tables[0], want)?;
            if before.3 != "AXRows" {
                return Err(KaxError::blocked("tail_rows_unsupported"));
            }
            if before.1 != count {
                return Err(KaxError::blocked("transcript_changed_during_read"));
            }
            let mut seen = IdentitySet::default();
            let mut result_rows = Vec::with_capacity(before.0.len());
            for (offset, row) in before.0.iter().enumerate() {
                if !seen.insert(row) {
                    return Err(KaxError::blocked("invalid_transcript_rows"));
                }
                result_rows.push(ctx.read_row_checked(row, before.2 + offset, true)?);
                ctx.progress();
            }
            let after = ctx.rows(&tables[0], want)?;
            let mut current_windows = Vec::new();
            for w in ctx.elements(&application, "AXWindows")? {
                if ctx.string(&w, "AXTitle", false)? == title {
                    current_windows.push(w);
                }
            }
            let unchanged = current_windows.len() == 1
                && ax_equal(&current_windows[0], &window)
                && ctx.string(&window, "AXTitle", false)? == title
                && before.1 == after.1
                && before.0.len() == after.0.len()
                && before.0.iter().zip(after.0.iter()).all(|(a, b)| ax_equal(a, b));
            if !unchanged {
                return Err(KaxError::blocked("transcript_changed_during_read"));
            }
            let mut m = Map::new();
            m.insert("status".to_string(), json!("ok"));
            m.insert("windowTitle".to_string(), json!(title));
            m.insert("processId".to_string(), json!(pid));
            m.insert("rows".to_string(), Value::Array(result_rows));
            m.insert("exposedRowCount".to_string(), json!(before.1));
            m.insert("rowStart".to_string(), json!(before.2));
            Ok(m)
        })();
        let map = match res {
            Ok(m) => m,
            Err(KaxError::Blocked(reason)) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m
            }
            Err(_) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("bridge_error"));
                m
            }
        };
        ctx.finalize_report(map)
    }

    fn run_read(action: &str, titles: &[String]) -> Value {
        let mut ctx = ReadCtx::new();
        let res: Result<Map<String, Value>> = (|| {
            let (application, pid) = Self::make_application(0.2)?;
            let windows = ctx.elements(&application, "AXWindows")?;
            if action == "windows" {
                let mut window_titles = Vec::with_capacity(windows.len());
                for w in &windows {
                    window_titles.push(ctx.string(w, "AXTitle", false)?);
                }
                let mut items = Vec::with_capacity(windows.len());
                for (w, title) in windows.iter().zip(window_titles.iter()) {
                    let matches = window_titles.iter().filter(|t| *t == title).count();
                    let minimized = ctx
                        .attribute(w, "AXMinimized", true)
                        .ok()
                        .flatten()
                        .and_then(|v| cf_to_bool(v.as_ptr()));
                    match ctx.controls(w) {
                        Ok((composers, tables)) => {
                            let is_main =
                                ctx.string(w, "AXIdentifier", true).unwrap_or_default() == "Main Window";
                            let transcript = readable_transcript_check(composers, tables.len());
                            let readable =
                                !is_main && !title.is_empty() && matches == 1 && transcript.is_ok();
                            let mut item = Map::new();
                            item.insert("windowTitle".to_string(), json!(title));
                            item.insert("titleMatches".to_string(), json!(matches));
                            item.insert("minimized".to_string(), json!(minimized));
                            item.insert("readable".to_string(), json!(readable));
                            // false: one-way channel window (no composer), readable but not sendable.
                            item.insert("hasComposer".to_string(), json!(composers == 1));
                            item.insert("composerCount".to_string(), json!(composers));
                            if matches > 1 {
                                item.insert("reason".to_string(), json!("target_room_window_ambiguous"));
                            } else if !is_main && transcript == Err("channel_window_unreadable") {
                                item.insert("reason".to_string(), json!("channel_window_unreadable"));
                            }
                            items.push(Value::Object(item));
                        }
                        Err(KaxError::Blocked(reason)) => {
                            items.push(json!({
                                "windowTitle": title,
                                "readable": false,
                                "reason": reason,
                            }));
                        }
                        Err(e) => return Err(e),
                    }
                }
                let mut m = Map::new();
                m.insert("status".to_string(), json!("ok"));
                m.insert("items".to_string(), Value::Array(items));
                m.insert("processId".to_string(), json!(pid));
                return Ok(m);
            }

            if titles.len() != 1 || titles[0].is_empty() {
                return Err(KaxError::blocked("exact_window_title_required"));
            }
            let target_title = &titles[0];
            let (window, tables) = Self::target_chat_window(&mut ctx, windows, target_title)?;
            let cap = if action == "read_all" { SEARCH_ROW_CAP } else { READ_ROW_CAP };
            let before = ctx.rows(&tables[0], cap)?;
            let mut seen = IdentitySet::default();
            let mut result_rows = Vec::with_capacity(before.0.len());
            // Sender facts need the layout of a few rows (see `macos_rows`); a full search pass
            // (`read_all`, up to 5000 rows) skips them.
            let with_layout = action == "read";
            let mut senders = SenderContext::default();
            let mut table_frame: Option<Option<CGRect>> = None;
            for (offset, row) in before.0.iter().enumerate() {
                if !seen.insert(row) {
                    return Err(KaxError::blocked("invalid_transcript_rows"));
                }
                let mut parsed = ctx.read_row_checked(row, before.2 + offset, true)?;
                if with_layout {
                    let parts = parsed.get("parts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    let shape = row_shape(&parts);
                    let need = senders.need(&shape);
                    let mut centered = None;
                    if need != LayoutNeed::None {
                        let table = *table_frame.get_or_insert_with(|| ctx.ax_frame(&tables[0]));
                        let msg = ctx.message_element(row).and_then(|el| ctx.ax_frame(&el));
                        if let (Some(t), Some(m)) = (table, msg) {
                            let key = if need == LayoutNeed::OwnSide { "ownSide" } else { "centered" };
                            let verdict = if need == LayoutNeed::OwnSide {
                                own_side_layout(t.origin.x, t.size.width, m.origin.x, m.size.width)
                            } else {
                                centered_layout(t.origin.x, t.size.width, m.origin.x, m.size.width)
                            };
                            if need == LayoutNeed::Centered {
                                centered = Some(verdict);
                            }
                            if let Some(o) = parsed.as_object_mut() {
                                o.insert(key.to_string(), json!(verdict));
                            }
                        }
                    }
                    senders.push(&shape, centered);
                }
                result_rows.push(parsed);
                ctx.progress();
            }
            let after = ctx.rows(&tables[0], cap)?;
            let mut current_windows = Vec::new();
            for w in ctx.elements(&application, "AXWindows")? {
                if ctx.string(&w, "AXTitle", false)? == *target_title {
                    current_windows.push(w);
                }
            }
            let unchanged = current_windows.len() == 1
                && ax_equal(&current_windows[0], &window)
                && ctx.string(&window, "AXTitle", false)? == *target_title
                && before.1 == after.1
                && before.0.len() == after.0.len()
                && before
                    .0
                    .iter()
                    .zip(after.0.iter())
                    .all(|(a, b)| ax_equal(a, b));
            if !unchanged {
                return Err(KaxError::blocked("transcript_changed_during_read"));
            }
            let mut m = Map::new();
            m.insert("status".to_string(), json!("ok"));
            m.insert("windowTitle".to_string(), json!(target_title));
            m.insert("processId".to_string(), json!(pid));
            m.insert("rows".to_string(), Value::Array(result_rows));
            m.insert("exposedRowCount".to_string(), json!(before.1));
            m.insert("rowStart".to_string(), json!(before.2));
            m.insert("rowsSource".to_string(), json!(before.3));
            m.insert("rowsTruncated".to_string(), json!(before.2 > 0));
            m.insert("rowLimit".to_string(), json!(cap));
            Ok(m)
        })();

        let map = match res {
            Ok(m) => m,
            Err(KaxError::Blocked(reason)) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m
            }
            Err(_) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("bridge_error"));
                m
            }
        };
        ctx.finalize_report(map)
    }

    fn run_send(
        action: &str,
        titles: &[String],
        text: Option<&str>,
        require_self: bool,
        window_identity: Option<&str>,
    ) -> Value {
        let mut ctx = SendCtx::new();
        let res: Result<Map<String, Value>> = (|| {
            if titles.is_empty() || !matches!(action, "inspect" | "raise" | "send") {
                return Err(KaxError::blocked("invalid_request"));
            }
            let (application, pid) = Self::make_application(0.25)?;
            let bound_self = if require_self && action == "send" {
                let token = window_identity
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| KaxError::blocked("self_room_not_verified"))?;
                let (expected_pid, expected_title, expected_window) =
                    lookup_verified_self_window(token)
                        .ok_or_else(|| KaxError::blocked("self_room_not_verified"))?;
                if expected_pid != pid || !titles.iter().any(|t| t == &expected_title) {
                    return Err(KaxError::blocked("target_room_window_changed"));
                }
                Some(expected_window)
            } else {
                None
            };
            let windows = ControlCtx::elements(&application, "AXWindows");
            let matches: Vec<CfRef> = windows
                .into_iter()
                .filter(|w| {
                    let t = ControlCtx::string(w, "AXTitle");
                    titles.iter().any(|cand| cand == &t)
                })
                .collect();
            if let Some(ref expected) = bound_self {
                verify_bound_self_window(&matches, Some(expected), ax_equal)?;
            } else if matches.len() != 1 {
                return Err(KaxError::blocked(if matches.is_empty() {
                    "target_room_window_not_open"
                } else {
                    "target_room_window_ambiguous"
                }));
            }
            let window = matches.into_iter().next().unwrap();
            if !ControlCtx::elements(&window, "AXSheets").is_empty() {
                return Err(KaxError::blocked("modal_dialog_present"));
            }
            if action == "raise" {
                let raise_act = cf_string("AXRaise");
                if !ControlCtx::action_names(&window).iter().any(|a| a == "AXRaise")
                    || unsafe { AXUIElementPerformAction(window.as_ptr(), raise_act.as_ptr()) }
                        != K_AX_ERROR_SUCCESS
                {
                    return Err(KaxError::blocked("target_window_raise_failed"));
                }
                let mut m = Map::new();
                m.insert("status".to_string(), json!("window_raised"));
                return Ok(m);
            }
            let (composer, _) = ctx.controls(&window)?;
            let draft = ControlCtx::composer_text(&composer)?;
            if !draft.is_empty() {
                return Err(KaxError::blocked("existing_draft_preserved"));
            }
            if action == "inspect" {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("ready"));
                m.insert("composerFound".to_string(), json!(true));
                m.insert("sendAction".to_string(), json!("AXPress"));
                m.insert("draftPresent".to_string(), json!(false));
                return Ok(m);
            }
            let Some(body) = text else {
                return Err(KaxError::blocked("invalid_message_text"));
            };
            if body.trim().is_empty() || body.len() > 16_000 {
                return Err(KaxError::blocked("invalid_message_text"));
            }
            let val_attr = cf_string("AXValue");
            let val_cf = cf_string(body);
            let set_rc = unsafe {
                AXUIElementSetAttributeValue(composer.as_ptr(), val_attr.as_ptr(), val_cf.as_ptr())
            };
            if set_rc != K_AX_ERROR_SUCCESS {
                return Err(KaxError::blocked(format!("composer_set_failed_{set_rc}")));
            }
            ctx.composer_changed = true;
            if ControlCtx::composer_text(&composer)? != body {
                return Err(KaxError::blocked("composer_value_mismatch"));
            }
            let (mut cur_composer, mut cur_button) = ctx.controls(&window)?;
            for _ in 0..10 {
                if ControlCtx::attr(&cur_button, "AXEnabled")
                    .and_then(|v| cf_to_bool(v.as_ptr()))
                    == Some(true)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
                let pair = ctx.controls(&window)?;
                cur_composer = pair.0;
                cur_button = pair.1;
            }
            let cur_title = ControlCtx::string(&window, "AXTitle");
            let enabled = ControlCtx::attr(&cur_button, "AXEnabled")
                .and_then(|v| cf_to_bool(v.as_ptr()))
                == Some(true);
            if !titles.iter().any(|t| t == &cur_title)
                || ControlCtx::composer_text(&cur_composer)? != body
                || !enabled
            {
                return Err(KaxError::blocked("send_precondition_changed"));
            }
            if let Some(ref expected) = bound_self {
                let current_matches: Vec<CfRef> = ControlCtx::elements(&application, "AXWindows")
                    .into_iter()
                    .filter(|w| {
                        let t = ControlCtx::string(w, "AXTitle");
                        titles.iter().any(|cand| cand == &t)
                    })
                    .collect();
                verify_bound_self_window(&current_matches, Some(expected), ax_equal)?;
                if !ax_equal(&window, expected) {
                    return Err(KaxError::blocked("target_room_window_changed"));
                }
            }
            ctx.dispatch_attempted = true;
            let press = cf_string("AXPress");
            let rc = unsafe { AXUIElementPerformAction(cur_button.as_ptr(), press.as_ptr()) };
            let mut m = Map::new();
            m.insert(
                "status".to_string(),
                json!(if rc == K_AX_ERROR_SUCCESS {
                    "dispatch_attempted"
                } else {
                    "dispatch_result_unknown"
                }),
            );
            m.insert("axResult".to_string(), json!(rc));
            m.insert("actions".to_string(), json!(["AXValue", "AXPress"]));
            Ok(m)
        })();

        let map = match res {
            Ok(m) => m,
            Err(KaxError::Blocked(reason)) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m
            }
            Err(_) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("bridge_error"));
                m
            }
        };
        ctx.finalize_report(map)
    }

    fn run_control(command: &str, args: &Map<String, Value>) -> Value {
        let mut ctx = ControlCtx::new();
        let res: Result<Map<String, Value>> = (|| {
            if !matches!(
                command,
                "rooms"
                    | "open"
                    | "close"
                    | "load_older"
                    | "search_open"
                    | "search_query"
                    | "search_previous"
                    | "search_close"
                    | "search_rooms"
                    | "members"
                    | "folders"
            ) {
                return Err(KaxError::blocked("invalid_control_request"));
            }
            let (application, pid) = Self::make_application(0.2)?;
            let title = args
                .get("windowTitle")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if matches!(command, "rooms" | "open" | "search_rooms" | "folders") {
                let main = ctx.ensure_chats(&application)?;
                if command == "folders" {
                    return ctx.folders_report(&main);
                }
                if command == "search_rooms" {
                    let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
                        return Err(KaxError::blocked("invalid_query"));
                    };
                    if query.len() > 4096 {
                        return Err(KaxError::blocked("invalid_query"));
                    }
                    // An empty query clears the search; the bar is closed too (back to the plain
                    // room list) unless the caller restores a bar that was open before.
                    let keep_open = args.get("keepOpen") == Some(&Value::Bool(true));
                    // The list reloads asynchronously after the query changes: remember what it
                    // showed so the read below waits for the new results instead of a fixed delay.
                    let table = ctx.room_table(&main)?;
                    let before = ctx.room_table_signature(&table);
                    let current_query = ctx.room_search_query(&main)?;
                    let search_open = !ctx.room_search_fields(&main)?.is_empty();
                    let changes = if query.is_empty() && !keep_open {
                        let was_open = search_open;
                        ctx.close_room_search(&main)?;
                        was_open && !current_query.is_empty()
                    } else {
                        let field = ctx.search_field(&main, true)?;
                        ctx.set_search_value(&field, query)?;
                        if ControlCtx::action_names(&field).iter().any(|a| a == "AXConfirm") {
                            ctx.act(&field, "AXConfirm")?;
                        }
                        current_query != query
                    };
                    if changes {
                        ctx.wait_room_table_reload(&table, &before);
                    }
                }
                if command != "open" {
                    if let Some(folder) = args.get("folder").and_then(|v| v.as_str()) {
                        if command != "rooms" {
                            return Err(KaxError::blocked("invalid_control_request"));
                        }
                        return ctx.rooms_in_folder(&main, folder, args);
                    }
                    return ctx.rooms_report(&main, args);
                }
                let require_self = args.get("requireSelf") == Some(&Value::Bool(true));
                let names_only = !require_self;
                let (entries, _reported, _rows_source) = ctx.room_entries(&main, names_only)?;

                if title.is_empty() {
                    return Err(KaxError::blocked("exact_window_title_required"));
                }
                // Room-list names may carry a room-type prefix ("오픈채팅, …") that the chat
                // window title does not; compare and verify on the prefix-free name.
                // A type label in the request ("팀채팅, A") must match the list entry's type.
                let names: Vec<&str> = entries
                    .iter()
                    .map(|(_, info)| info.get("name").and_then(|v| v.as_str()).unwrap_or(""))
                    .collect();
                let found = match_room_name(title, &names);
                let title = found.base;
                let matching: Vec<&(CfRef, Value)> =
                    found.matching.iter().map(|&i| &entries[i]).collect();
                if matching.len() > 1 {
                    return Err(KaxError::blocked("target_room_name_ambiguous"));
                }
                if found.typed && matching.is_empty() {
                    // Not visible with that type: let the caller try the app's room search, and
                    // never mistake a same-named window of another type for this room.
                    let mut m = Map::new();
                    m.insert("status".to_string(), json!("blocked"));
                    m.insert("reason".to_string(), json!("room_not_in_exposed_list"));
                    m.insert("roomTypeMismatch".to_string(), json!(found.other_type > 0));
                    return Ok(m);
                }
                // Window titles carry no type label, so with a same-named room of another type in
                // the list an open window titled `title` cannot be told apart.
                let other_type_listed = found.typed && found.other_type > 0;
                if require_self {
                    if matching.is_empty() {
                        return ctx.verify_self_via_friends_menu(&application, pid, title);
                    }
                    if matching[0].1.get("isSelf") != Some(&Value::Bool(true)) {
                        return Err(KaxError::blocked("self_room_not_verified"));
                    }
                }
                let before_all = ControlCtx::elements(&application, "AXWindows");
                let opened: Vec<CfRef> = before_all
                    .iter()
                    .filter(|w| {
                        ControlCtx::string(w, "AXIdentifier") != "Main Window"
                            && ControlCtx::string(w, "AXTitle") == title
                    })
                    .cloned()
                    .collect();
                if !require_self && !opened.is_empty() {
                    if other_type_listed {
                        return Err(KaxError::blocked("target_room_window_ambiguous"));
                    }
                    let _ = ctx.window(&application, title)?;
                    let mut m = Map::new();
                    m.insert("status".to_string(), json!("ok"));
                    m.insert("windowTitle".to_string(), json!(title));
                    m.insert("alreadyOpen".to_string(), json!(true));
                    return Ok(m);
                }
                if require_self && opened.len() > 1 {
                    return Err(KaxError::blocked("target_room_window_ambiguous"));
                }
                let existing_target = if require_self {
                    opened.into_iter().next()
                } else {
                    None
                };
                let before_chats: Vec<CfRef> = before_all
                    .into_iter()
                    .filter(|w| ControlCtx::string(w, "AXIdentifier") != "Main Window")
                    .collect();
                if matching.len() != 1 {
                    return Err(KaxError::blocked("room_not_in_exposed_list"));
                }
                let focused_before = if require_self {
                    Some(ctx.focus_main_for_self_menu(&application, &main)?)
                } else {
                    None
                };
                let cell = &matching[0].0;
                if ControlCtx::elements(cell, "AXChildren")
                    .iter()
                    .any(|c| ControlCtx::string(c, "AXRole") == "AXMenu")
                {
                    return Err(KaxError::blocked("existing_menu_preserved"));
                }
                ctx.act(cell, "AXShowMenu")?;
                let mut open_items: Vec<CfRef>;
                let menu_deadline = ctx.deadline.min(Instant::now() + Duration::from_secs(1));
                loop {
                    let menus: Vec<CfRef> = ControlCtx::elements(cell, "AXChildren")
                        .into_iter()
                        .filter(|c| ControlCtx::string(c, "AXRole") == "AXMenu")
                        .collect();
                    open_items = menus
                        .iter()
                        .flat_map(|m| ControlCtx::elements(m, "AXChildren"))
                        .filter(|item| {
                            ControlCtx::string(item, "AXRole") == "AXMenuItem"
                                && ControlCtx::string(item, "AXIdentifier") == "openChatRoom:"
                                && ControlCtx::attr(item, "AXEnabled")
                                    .and_then(|v| cf_to_bool(v.as_ptr()))
                                    == Some(true)
                        })
                        .collect();
                    if open_items.len() == 1 || Instant::now() >= menu_deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(30));
                }
                if open_items.len() != 1 {
                    return Err(KaxError::blocked("open_menu_item_not_unique"));
                }
                ctx.act(&open_items[0], "AXPress")?;
                let until = ctx.deadline.min(Instant::now() + Duration::from_secs(2));
                if let Some(focused_before) = focused_before {
                    let (candidate, already_open) = ctx.observe_self_menu_window(
                        &application, &main, &focused_before, &before_chats,
                    )?;
                    let actual_title = ControlCtx::string(&candidate, "AXTitle");
                    if actual_title != title {
                        return Err(KaxError::blocked("self_room_not_verified"));
                    }
                    let same_title_cnt = ControlCtx::elements(&application, "AXWindows")
                        .into_iter()
                        .filter(|w| ControlCtx::string(w, "AXTitle") == actual_title)
                        .count();
                    if same_title_cnt != 1 {
                        return Err(KaxError::blocked("target_room_window_ambiguous"));
                    }
                    if let Some(ref exp) = existing_target {
                        if !ax_equal(&candidate, exp) {
                            return Err(KaxError::blocked("self_room_not_verified"));
                        }
                    }
                    let token = register_verified_self_window(pid, &actual_title, candidate);
                    let mut m = Map::new();
                    m.insert("status".to_string(), json!("ok"));
                    m.insert("windowTitle".to_string(), json!(title));
                    m.insert("alreadyOpen".to_string(), json!(already_open));
                    m.insert("isSelf".to_string(), json!(true));
                    m.insert("windowIdentity".to_string(), json!(token));
                    return Ok(m);
                }
                let mut last_reason = String::from("opened_window_not_observed");
                // A normal chat window may show its transcript before its composer exists, so a
                // window without a composer counts as a one-way channel window only after it has
                // kept that shape for a while (or until the wait ends).
                let mut one_way_since: Option<Instant> = None;
                let opened_ok = |one_way: bool| {
                    let mut m = Map::new();
                    m.insert("status".to_string(), json!("ok"));
                    m.insert("windowTitle".to_string(), json!(title));
                    m.insert("alreadyOpen".to_string(), json!(false));
                    if one_way {
                        m.insert("hasComposer".to_string(), json!(false));
                    }
                    m
                };
                while Instant::now() < until {
                    match ctx.window_shape(&application, title) {
                        Ok((_, true)) => return Ok(opened_ok(false)),
                        Ok((_, false)) => {
                            let since = *one_way_since.get_or_insert_with(Instant::now);
                            if since.elapsed() >= ONE_WAY_WINDOW_SETTLE {
                                return Ok(opened_ok(true));
                            }
                        }
                        Err(KaxError::Blocked(reason)) => {
                            one_way_since = None;
                            last_reason = reason;
                        }
                        Err(e) => return Err(e),
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if one_way_since.is_some() {
                    return Ok(opened_ok(true));
                }
                // A window with the title that was not there before may still have opened even
                // though it is not a usable chat; say so, so callers can close what they opened.
                let window_opened = ControlCtx::elements(&application, "AXWindows")
                    .iter()
                    .any(|w| {
                        ControlCtx::string(w, "AXIdentifier") != "Main Window"
                            && ControlCtx::string(w, "AXTitle") == title
                            && !before_chats.iter().any(|b| ax_equal(b, w))
                    });
                let reason = if window_opened && last_reason != "target_room_window_not_open" {
                    last_reason
                } else {
                    String::from("opened_window_not_observed")
                };
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m.insert("windowTitle".to_string(), json!(title));
                m.insert("windowOpened".to_string(), json!(window_opened));
                return Ok(m);
            }

            if title.is_empty() {
                return Err(KaxError::blocked("exact_window_title_required"));
            }
            if command == "close"
                && !ControlCtx::elements(&application, "AXWindows")
                    .iter()
                    .any(|w| ControlCtx::string(w, "AXTitle") == title)
            {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("ok"));
                m.insert("windowTitle".to_string(), json!(title));
                m.insert("alreadyClosed".to_string(), json!(true));
                return Ok(m);
            }
            if command == "close" {
                return ctx.close_window(&application, title);
            }
            let target = ctx.window(&application, title)?;
            if command == "members" {
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .filter(|l| (1..=MAX_MEMBER_LIMIT as u64).contains(l))
                    .ok_or_else(|| KaxError::blocked("invalid_control_request"))?;
                let mut m = ctx.read_members(&target, limit as usize)?;
                m.insert("windowTitle".to_string(), json!(title));
                return Ok(m);
            }
            let controls = ctx.chat_controls(&target)?;

            if command == "search_close" {
                let fields: Vec<&CfRef> = controls
                    .iter()
                    .filter(|n| {
                        matches!(
                            ControlCtx::string(n, "AXRole").as_str(),
                            "AXTextField" | "AXSearchField"
                        )
                    })
                    .collect();
                if fields.is_empty() {
                    let mut m = Map::new();
                    m.insert("status".to_string(), json!("ok"));
                    m.insert("alreadyClosed".to_string(), json!(true));
                    m.insert("windowTitle".to_string(), json!(title));
                    return Ok(m);
                }
                let buttons: Vec<&CfRef> = controls
                    .iter()
                    .filter(|n| {
                        ControlCtx::string(n, "AXRole") == "AXButton"
                            && matches!(
                                ControlCtx::string(n, "AXDescription").as_str(),
                                "검색" | "Search"
                            )
                    })
                    .collect();
                if buttons.len() != 1 {
                    return Err(KaxError::blocked("search_button_not_unique"));
                }
                ctx.act(buttons[0], "AXPress")?;
                let until = ctx.deadline.min(Instant::now() + Duration::from_secs(1));
                while Instant::now() < until {
                    let current = ctx
                        .nodes(&target, false, 0)?
                        .into_iter()
                        .filter(|n| {
                            matches!(
                                ControlCtx::string(n, "AXRole").as_str(),
                                "AXTextField" | "AXSearchField"
                            )
                        })
                        .count();
                    if current == 0 {
                        let mut m = Map::new();
                        m.insert("status".to_string(), json!("ok"));
                        m.insert("alreadyClosed".to_string(), json!(false));
                        m.insert("windowTitle".to_string(), json!(title));
                        return Ok(m);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                return Err(KaxError::blocked("search_close_not_observed"));
            }

            if matches!(command, "search_open" | "search_query" | "search_previous") {
                let field = ctx.search_field(&target, command != "search_previous")?;
                if command == "search_open" {
                    let mut m = Map::new();
                    m.insert("status".to_string(), json!("ok"));
                    m.insert("windowTitle".to_string(), json!(title));
                    return Ok(m);
                }
                let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
                    return Err(KaxError::blocked("invalid_query"));
                };
                if query.is_empty() || query.len() > 4096 {
                    return Err(KaxError::blocked("invalid_query"));
                }
                let before = ctx.search_state(&target, &field, query)?;
                let before_key = serde_json::to_string(&before).unwrap_or_default();
                if command == "search_previous" {
                    if ControlCtx::string(&field, "AXValue") != query {
                        return Err(KaxError::blocked("search_query_changed"));
                    }
                    if before.get("previousEnabled") == Some(&Value::Bool(false)) {
                        let mut m = Map::new();
                        m.insert("status".to_string(), json!("ok"));
                        m.insert("searchVerified".to_string(), json!(true));
                        m.insert("searchOutcome".to_string(), json!("navigation_exhausted"));
                        m.insert("searchState".to_string(), before);
                        return Ok(m);
                    }
                    let btns = ControlCtx::search_buttons(&field)?;
                    ctx.act(&btns[0], "AXPress")?;
                } else {
                    ctx.set_search_value(&field, query)?;
                    if !ControlCtx::action_names(&field).iter().any(|a| a == "AXConfirm") {
                        return Err(KaxError::blocked("search_confirm_unavailable"));
                    }
                    ctx.act(&field, "AXConfirm")?;
                }

                let ver_started = Instant::now();
                let until = (ctx.deadline - Duration::from_millis(200))
                    .min(ver_started + Duration::from_secs(3));
                let mut last_key = String::new();
                let mut stable_samples = 0usize;
                let mut observed = before;
                while Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(150));
                    observed = ctx.search_state(&target, &field, query)?;
                    let key = serde_json::to_string(&observed).unwrap_or_default();
                    if key == last_key {
                        stable_samples += 1;
                    } else {
                        stable_samples = 0;
                    }
                    last_key = key.clone();
                    let moved = command != "search_previous" || key != before_key;
                    let match_cnt = observed
                        .get("visibleMatchCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if ver_started.elapsed() >= Duration::from_millis(650)
                        && stable_samples >= 1
                        && moved
                        && observed.get("query").and_then(|v| v.as_str()) == Some(query)
                        && match_cnt > 0
                    {
                        let mut m = Map::new();
                        m.insert("status".to_string(), json!("ok"));
                        m.insert("windowTitle".to_string(), json!(title));
                        m.insert("query".to_string(), json!(query));
                        m.insert("searchVerified".to_string(), json!(true));
                        m.insert("searchOutcome".to_string(), json!("visible_match_observed"));
                        m.insert("searchState".to_string(), observed);
                        return Ok(m);
                    }
                }
                let mut m = Map::new();
                m.insert("status".to_string(), json!("partial"));
                m.insert("reason".to_string(), json!("search_result_not_observed"));
                m.insert("windowTitle".to_string(), json!(title));
                m.insert("query".to_string(), json!(query));
                m.insert("searchVerified".to_string(), json!(false));
                m.insert("searchOutcome".to_string(), json!("unverified"));
                m.insert("searchState".to_string(), observed);
                return Ok(m);
            }

            // load_older
            let tables: Vec<&CfRef> = controls
                .iter()
                .filter(|n| ControlCtx::string(n, "AXRole") == "AXTable")
                .collect();
            if tables.len() != 1 {
                return Err(KaxError::blocked("chat_transcript_not_unique"));
            }
            let scrolls: Vec<&CfRef> = controls
                .iter()
                .filter(|el| {
                    ControlCtx::string(el, "AXRole") == "AXScrollArea"
                        && ControlCtx::elements(el, "AXChildren")
                            .iter()
                            .any(|c| ax_equal(c, tables[0]))
                })
                .collect();
            if scrolls.len() != 1
                || !ControlCtx::action_names(scrolls[0])
                    .iter()
                    .any(|a| a == "AXScrollUpByPage")
            {
                return Err(KaxError::blocked("history_scroll_not_supported"));
            }
            let before_cnt = ControlCtx::reported_row_count(tables[0])
                .ok_or_else(|| KaxError::blocked("transcript_rows_unavailable"))?;
            let bar = ControlCtx::attr(scrolls[0], "AXVerticalScrollBar").or_else(|| {
                ControlCtx::elements(scrolls[0], "AXChildren")
                    .into_iter()
                    .find(|c| ControlCtx::string(c, "AXRole") == "AXScrollBar")
            });
            let mut method = "AXScrollUpByPage";
            if let Some(scrollbar) = bar {
                if ControlCtx::settable(&scrollbar, "AXValue") {
                    let min_val = ControlCtx::attr(&scrollbar, "AXMinValue")
                        .unwrap_or_else(|| cf_number_f64(0.0));
                    let attr = cf_string("AXValue");
                    let rc = unsafe {
                        AXUIElementSetAttributeValue(
                            scrollbar.as_ptr(),
                            attr.as_ptr(),
                            min_val.as_ptr(),
                        )
                    };
                    ctx.actions
                        .push(json!({"action": "AXValue(scrollbar_minimum)", "result": rc}));
                    if rc != K_AX_ERROR_SUCCESS {
                        return Err(KaxError::blocked("history_scrollbar_set_failed"));
                    }
                    method = "AXValue(scrollbar_minimum)";
                } else {
                    let prev: Vec<CfRef> = ControlCtx::elements(&scrollbar, "AXChildren")
                        .into_iter()
                        .filter(|c| {
                            matches!(
                                ControlCtx::string(c, "AXSubrole").as_str(),
                                "AXDecrementPage" | "AXDecrementArrow"
                            )
                        })
                        .collect();
                    if prev.len() == 1 {
                        ctx.act(&prev[0], "AXPress")?;
                        method = "AXPress(scrollbar_previous)";
                    } else {
                        ctx.act(scrolls[0], "AXScrollUpByPage")?;
                    }
                }
            } else {
                ctx.act(scrolls[0], "AXScrollUpByPage")?;
            }
            // Wait for the older page to arrive: return as soon as the row count grows, and give
            // a slow load up to 1.5 s so callers that stop on "no new rows" are not misled.
            let wait_started = Instant::now();
            let wait_until = ctx.deadline.min(wait_started + Duration::from_millis(1500));
            std::thread::sleep(Duration::from_millis(100));
            // Count only (no row array copy); a failed count keeps the last known value instead
            // of reading as 0 rows.
            let mut after_cnt = ControlCtx::reported_row_count(tables[0]).unwrap_or(before_cnt);
            while after_cnt <= before_cnt && Instant::now() < wait_until {
                std::thread::sleep(Duration::from_millis(50));
                after_cnt = ControlCtx::reported_row_count(tables[0]).unwrap_or(after_cnt);
            }
            let action_ok = ctx
                .actions
                .last()
                .and_then(|a| a.get("result"))
                .and_then(|v| v.as_i64())
                == Some(0);
            let mut m = Map::new();
            m.insert("status".to_string(), json!("ok"));
            m.insert("windowTitle".to_string(), json!(title));
            m.insert("exposedRowsBefore".to_string(), json!(before_cnt));
            m.insert("exposedRowsAfter".to_string(), json!(after_cnt));
            m.insert("method".to_string(), json!(method));
            m.insert("scrollActionSucceeded".to_string(), json!(action_ok));
            m.insert(
                "loadStatus".to_string(),
                json!(if action_ok {
                    "history_requested"
                } else {
                    "no_scroll_effect_observed"
                }),
            );
            m.insert("historyComplete".to_string(), json!(false));
            m.insert("endOfHistory".to_string(), Value::Null);
            Ok(m)
        })();

        let map = match res {
            Ok(m) => m,
            Err(KaxError::Blocked(reason)) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m
            }
            Err(_) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("bridge_error"));
                m
            }
        };
        ctx.finalize_report(map)
    }

    fn run_attachment(
        window_title: &str,
        row_index: i64,
        expected_parts: &[Value],
        expected_count: usize,
        ocr: bool,
    ) -> Value {
        let mut ctx = ReadCtx::new();
        ctx.deadline = ctx.started + Duration::from_secs(18);

        let res: Result<Map<String, Value>> = (|| {
            let (application, pid) = Self::make_application(0.2)?;
            let windows = ctx.elements(&application, "AXWindows")?;
            let mut matches = Vec::new();
            for w in windows {
                if ctx.string(&w, "AXTitle", false)? == window_title {
                    matches.push(w);
                }
            }
            if matches.len() != 1 {
                return Err(KaxError::blocked(if matches.is_empty() {
                    "target_room_window_not_open"
                } else {
                    "target_room_window_ambiguous"
                }));
            }
            let window = matches.into_iter().next().unwrap();
            // The main window (room list table, no composer) must never pass as a one-way chat.
            if ctx.string(&window, "AXIdentifier", true)? == "Main Window" {
                return Err(KaxError::blocked("main_window_is_not_a_chat"));
            }
            let (composers, tables) = ctx.controls(&window)?;
            readable_transcript_check(composers, tables.len()).map_err(KaxError::blocked)?;
            let table = &tables[0];
            let listing = ctx.rows(table, SEARCH_ROW_CAP)?;
            let row_idx_u = row_index as usize;
            if listing.1 != expected_count || row_idx_u < listing.2 {
                return Err(KaxError::blocked("attachment_snapshot_changed"));
            }
            let local_idx = row_idx_u - listing.2;
            let Some(row) = listing.0.get(local_idx) else {
                return Err(KaxError::blocked("attachment_snapshot_changed"));
            };
            let actual = ctx.read_row(row, row_idx_u)?;
            let actual_parts = actual
                .get("parts")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if actual_parts != expected_parts {
                return Err(KaxError::blocked("attachment_snapshot_changed"));
            }
            let has_text = actual_parts.iter().any(|p| {
                p.get("role").and_then(|v| v.as_str()) == Some("AXTextArea")
                    && p.get("AXValue")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty())
            });
            if has_text {
                return Err(KaxError::blocked("attachment_requires_image_only_row"));
            }
            let mut images = Vec::new();
            for el in ctx.image_elements(row)? {
                let r = ctx.frame(&el)?;
                if r.size.width >= 40.0 && r.size.height >= 40.0 {
                    images.push(el);
                }
            }
            if images.len() != 1 {
                return Err(KaxError::blocked("attachment_image_not_unique"));
            }
            if !attachment_capture_available() {
                return Err(KaxError::blocked("attachment_capture_requires_macos14_2"));
            }
            if unsafe { CGPreflightScreenCaptureAccess() } == 0 {
                return Err(KaxError::blocked("screen_recording_permission_required"));
            }
            let scroll = ctx
                .attribute(table, "AXParent", false)?
                .ok_or_else(|| KaxError::blocked("transcript_scroll_unavailable"))?;
            if ctx.string(&scroll, "AXRole", false)? != "AXScrollArea" {
                return Err(KaxError::blocked("transcript_scroll_unavailable"));
            }
            let viewport = ctx.frame(&scroll)?;
            let scrollbar = ctx.attribute(&scroll, "AXVerticalScrollBar", true)?;
            let original_scroll = scrollbar
                .as_ref()
                .and_then(|b| ctx.attribute(b, "AXValue", true).ok().flatten())
                .and_then(|v| cf_to_f64(v.as_ptr()));
            let mut changed_scroll = false;
            let mut last_scroll: Option<f64> = None;

            let capture_res: Result<Map<String, Value>> = (|| {
                for _ in 0..4 {
                    let img_frame = ctx.frame(&images[0])?;
                    if viewport.inset_by(0.0, 2.0).contains(&img_frame) {
                        break;
                    }
                    let Some(ref bar) = scrollbar else {
                        return Err(KaxError::blocked("attachment_not_fully_visible"));
                    };
                    let current = ctx
                        .attribute(bar, "AXValue", false)?
                        .and_then(|v| cf_to_f64(v.as_ptr()))
                        .ok_or_else(|| KaxError::blocked("attachment_not_fully_visible"))?;
                    let (Some(first), Some(last)) = (listing.0.first(), listing.0.last()) else {
                        return Err(KaxError::blocked("attachment_not_fully_visible"));
                    };
                    if img_frame.size.height >= viewport.size.height - 4.0 {
                        return Err(KaxError::blocked("attachment_not_fully_visible"));
                    }
                    let content_h = ctx.frame(last)?.max_y() - ctx.frame(first)?.min_y();
                    if content_h <= viewport.size.height {
                        return Err(KaxError::blocked("attachment_not_fully_visible"));
                    }
                    let delta = img_frame.mid_y() - viewport.mid_y();
                    let next_val = (current + delta / (content_h - viewport.size.height)).clamp(0.0, 1.0);
                    let num = cf_number_f64(next_val);
                    let val_attr = cf_string("AXValue");
                    let rc = unsafe {
                        AXUIElementSetAttributeValue(bar.as_ptr(), val_attr.as_ptr(), num.as_ptr())
                    };
                    ctx.ui_actions += 1;
                    if rc != K_AX_ERROR_SUCCESS {
                        return Err(KaxError::blocked("attachment_scroll_failed"));
                    }
                    changed_scroll = true;
                    last_scroll = Some(next_val);
                    std::thread::sleep(Duration::from_millis(150));
                }

                let rect = ctx.frame(&images[0])?;
                if !viewport.contains(&rect) {
                    return Err(KaxError::blocked("attachment_not_fully_visible"));
                }
                let title = ctx.string(&window, "AXTitle", false)?;
                ensure_ns_application_shared();

                // ScreenCaptureKit: SCShareableContent.getShareableContentExcludingDesktopWindows:onScreenWindowsOnly:completionHandler:
                let (tx, rx) = mpsc::channel::<Option<usize>>();
                let content_block = RcBlock::new(move |content: *mut c_void, _err: *mut c_void| {
                    let retained = if content.is_null() {
                        None
                    } else {
                        unsafe { CFRetain(content as CFTypeRef) };
                        Some(content as usize)
                    };
                    let _ = tx.send(retained);
                });
                unsafe {
                    let sc_cls = cls("SCShareableContent");
                    let f: extern "C" fn(*mut c_void, *mut c_void, u8, u8, *const c_void) =
                        std::mem::transmute(objc_msgSend as *const ());
                    f(
                        sc_cls,
                        sel("getShareableContentExcludingDesktopWindows:onScreenWindowsOnly:completionHandler:"),
                        1,
                        0,
                        &*content_block as *const _ as *const c_void,
                    );
                }
                let content_ptr = rx
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|_| KaxError::blocked("attachment_capture_timeout"))?
                    .ok_or_else(|| KaxError::blocked("attachment_capture_window_not_unique"))?;
                let _content_guard = CfRef(content_ptr as CFTypeRef);

                let (capture_win, window_rect) = unsafe {
                    let get_wins: extern "C" fn(*mut c_void, *mut c_void) -> CFArrayRef =
                        std::mem::transmute(objc_msgSend as *const ());
                    let wins = get_wins(content_ptr as *mut c_void, sel("windows"));
                    let mut cands = Vec::new();
                    if !wins.is_null() {
                        let cnt = CFArrayGetCount(wins);
                        let get_app: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                            std::mem::transmute(objc_msgSend as *const ());
                        let get_pid: extern "C" fn(*mut c_void, *mut c_void) -> i32 =
                            std::mem::transmute(objc_msgSend as *const ());
                        let get_title: extern "C" fn(*mut c_void, *mut c_void) -> CFStringRef =
                            std::mem::transmute(objc_msgSend as *const ());
                        for i in 0..cnt {
                            let w = CFArrayGetValueAtIndex(wins, i) as *mut c_void;
                            let app = get_app(w, sel("owningApplication"));
                            if app.is_null() || get_pid(app, sel("processID")) != pid {
                                continue;
                            }
                            let t = cf_to_string(get_title(w, sel("title"))).unwrap_or_default();
                            if t == title {
                                cands.push(w);
                            }
                        }
                    }
                    if cands.len() != 1 {
                        return Err(KaxError::blocked("attachment_capture_window_not_unique"));
                    }
                    let w = cands[0];
                    (w, sc_window_frame(w))
                };

                if !window_rect.contains(&rect) {
                    return Err(KaxError::blocked("attachment_not_fully_visible"));
                }

                let captured_img: CGImageRef = unsafe {
                    let alloc: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                        std::mem::transmute(objc_msgSend as *const ());
                    let filter_alloc = alloc(cls("SCContentFilter"), sel("alloc"));
                    let init_filter: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
                        std::mem::transmute(objc_msgSend as *const ());
                    let filter = init_filter(
                        filter_alloc,
                        sel("initWithDesktopIndependentWindow:"),
                        capture_win,
                    );
                    let _filter_guard = CfRef(filter as CFTypeRef);

                    let cfg = alloc(alloc(cls("SCStreamConfiguration"), sel("alloc")), sel("init"));
                    let _cfg_guard = CfRef(cfg as CFTypeRef);
                    let set_usize: extern "C" fn(*mut c_void, *mut c_void, usize) =
                        std::mem::transmute(objc_msgSend as *const ());
                    let set_bool: extern "C" fn(*mut c_void, *mut c_void, u8) =
                        std::mem::transmute(objc_msgSend as *const ());
                    set_usize(cfg, sel("setWidth:"), (window_rect.size.width * 2.0) as usize);
                    set_usize(cfg, sel("setHeight:"), (window_rect.size.height * 2.0) as usize);
                    set_bool(cfg, sel("setShowsCursor:"), 0);
                    set_bool(cfg, sel("setScalesToFit:"), 1);
                    set_bool(cfg, sel("setIgnoreShadowsSingleWindow:"), 1);

                    let (tx2, rx2) = mpsc::channel::<Option<usize>>();
                    let cap_block = RcBlock::new(move |img: CGImageRef, _err: *mut c_void| {
                        let retained = if img.is_null() {
                            None
                        } else {
                            CGImageRetain(img);
                            Some(img as usize)
                        };
                        let _ = tx2.send(retained);
                    });
                    let sc_mgr = cls("SCScreenshotManager");
                    let cap_fn: extern "C" fn(
                        *mut c_void,
                        *mut c_void,
                        *mut c_void,
                        *mut c_void,
                        *const c_void,
                    ) = std::mem::transmute(objc_msgSend as *const ());
                    cap_fn(
                        sc_mgr,
                        sel("captureImageWithFilter:configuration:completionHandler:"),
                        filter,
                        cfg,
                        &*cap_block as *const _ as *const c_void,
                    );
                    rx2.recv_timeout(Duration::from_secs(3))
                        .map_err(|_| KaxError::blocked("attachment_window_capture_failed"))?
                        .ok_or_else(|| KaxError::blocked("attachment_window_capture_failed"))?
                        as CGImageRef
                };
                let cap_guard = CfRef(captured_img);

                let cap_w = unsafe { CGImageGetWidth(cap_guard.as_ptr()) } as f64;
                let cap_h = unsafe { CGImageGetHeight(cap_guard.as_ptr()) } as f64;
                let scale_x = cap_w / window_rect.size.width;
                let scale_y = cap_h / window_rect.size.height;
                let crop_rect = CGRect {
                    origin: CGPoint {
                        x: (rect.min_x() - window_rect.min_x()) * scale_x,
                        y: (rect.min_y() - window_rect.min_y()) * scale_y,
                    },
                    size: CGSize {
                        width: rect.size.width * scale_x,
                        height: rect.size.height * scale_y,
                    },
                }
                .integral();
                let crop = unsafe { CGImageCreateWithImageInRect(cap_guard.as_ptr(), crop_rect) };
                if crop.is_null() {
                    return Err(KaxError::blocked("attachment_crop_failed"));
                }
                let crop_guard = CfRef(crop);

                let cur_frame = ctx.frame(&images[0])?;
                let cur_row_parts = ctx
                    .read_row(row, row_idx_u)?
                    .get("parts")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let cur_rows = ctx.rows(table, SEARCH_ROW_CAP)?;
                if cur_frame != rect || cur_row_parts != expected_parts || cur_rows.1 != expected_count {
                    return Err(KaxError::blocked("attachment_changed_during_capture"));
                }

                let crop_w = unsafe { CGImageGetWidth(crop_guard.as_ptr()) };
                let crop_h = unsafe { CGImageGetHeight(crop_guard.as_ptr()) };
                let png = encode_cgimage_png(crop_guard.as_ptr())?;
                let mut m = Map::new();
                m.insert("status".to_string(), json!("ok"));
                m.insert("windowTitle".to_string(), json!(title));
                m.insert("rowIndex".to_string(), json!(row_index));
                m.insert("imageSource".to_string(), json!("ax_located_window_crop"));
                m.insert("originalImage".to_string(), json!(false));
                m.insert("width".to_string(), json!(crop_w));
                m.insert("height".to_string(), json!(crop_h));
                m.insert("ocrPerformed".to_string(), json!(false));
                if ocr {
                    let ocr_val = vision_ocr(crop_guard.as_ptr())?;
                    m.insert("ocrPerformed".to_string(), json!(true));
                    m.insert("ocr".to_string(), ocr_val);
                }
                m.insert(
                    "imageBase64".to_string(),
                    json!(base64::engine::general_purpose::STANDARD.encode(&png)),
                );
                Ok(m)
            })();

            // Restore scroll position if the user did not concurrently scroll.
            if changed_scroll {
                if let (Some(ref bar), Some(orig), Some(last)) =
                    (&scrollbar, original_scroll, last_scroll)
                {
                    if let Ok(Some(cur_ref)) = ctx.attribute(bar, "AXValue", true) {
                        if let Some(cur) = cf_to_f64(cur_ref.as_ptr()) {
                            if (cur - last).abs() < 0.002 {
                                let num = cf_number_f64(orig);
                                let attr = cf_string("AXValue");
                                unsafe {
                                    AXUIElementSetAttributeValue(
                                        bar.as_ptr(),
                                        attr.as_ptr(),
                                        num.as_ptr(),
                                    );
                                }
                                ctx.ui_actions += 1;
                            }
                        }
                    }
                }
            }
            capture_res
        })();

        let map = match res {
            Ok(m) => m,
            Err(KaxError::Blocked(reason)) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("blocked"));
                m.insert("reason".to_string(), json!(reason));
                m
            }
            Err(_) => {
                let mut m = Map::new();
                m.insert("status".to_string(), json!("bridge_error"));
                m
            }
        };
        ctx.finalize_report(map)
    }
}

impl AxDriver for MacosAxDriver {
    fn call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        if matches!(action, "read" | "read_all" | "windows") {
            Ok(Self::run_read(action, titles))
        } else {
            Ok(Self::run_send(action, titles, text, false, None))
        }
    }

    fn send_with_identity(
        &self,
        titles: &[String],
        text: &str,
        require_self: bool,
        window_identity: Option<&str>,
    ) -> Result<Value> {
        Ok(Self::run_send(
            "send",
            titles,
            Some(text),
            require_self,
            window_identity,
        ))
    }

    fn control(&self, action: &str, args: &Map<String, Value>) -> Result<Value> {
        Ok(Self::run_control(action, args))
    }

    fn read_tail(&self, window_title: &str, start: Option<usize>, last: usize) -> Option<Result<Value>> {
        Some(Ok(Self::run_read_tail(window_title, start, last)))
    }

    fn capture_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        expected_parts: &[Value],
        expected_count: usize,
        ocr: bool,
    ) -> Result<Value> {
        Ok(Self::run_attachment(
            window_title,
            row_index,
            expected_parts,
            expected_count,
            ocr,
        ))
    }
}
