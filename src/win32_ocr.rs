//! Windows.Media.Ocr via the WinRT COM ABI (`combase.dll`), zero external dependencies.

#![cfg(target_os = "windows")]

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::sync::OnceLock;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

use crate::error::{KaxError, Result};
use crate::win32_api::{until_bool, OcrBackend};
use crate::win32_image::Frame;
use crate::win32_rows::OcrLine;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRtRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

struct CombaseApi {
    ro_initialize: unsafe extern "system" fn(u32) -> i32,
    ro_uninitialize: unsafe extern "system" fn(),
    ro_get_activation_factory:
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    windows_create_string:
        unsafe extern "system" fn(*const u16, u32, *mut *mut c_void) -> i32,
    windows_delete_string: unsafe extern "system" fn(*mut c_void) -> i32,
    windows_get_string_raw_buffer:
        unsafe extern "system" fn(*mut c_void, *mut u32) -> *const u16,
}

fn combase() -> Result<&'static CombaseApi> {
    static API: OnceLock<std::result::Result<CombaseApi, String>> = OnceLock::new();
    let res = API.get_or_init(|| unsafe {
        let module = LoadLibraryA(b"combase.dll\0".as_ptr());
        if module.is_null() {
            return Err("windows_ocr_combase_unavailable".to_string());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let p = GetProcAddress(module, $name.as_ptr());
                if p.is_none() {
                    return Err("windows_ocr_combase_unavailable".to_string());
                }
                std::mem::transmute(p.unwrap())
            }};
        }
        Ok(CombaseApi {
            ro_initialize: sym!(b"RoInitialize\0"),
            ro_uninitialize: sym!(b"RoUninitialize\0"),
            ro_get_activation_factory: sym!(b"RoGetActivationFactory\0"),
            windows_create_string: sym!(b"WindowsCreateString\0"),
            windows_delete_string: sym!(b"WindowsDeleteString\0"),
            windows_get_string_raw_buffer: sym!(b"WindowsGetStringRawBuffer\0"),
        })
    });
    res.as_ref().map_err(|e| KaxError::blocked(e.clone()))
}

const IID_OCR_ENGINE_STATICS: Guid = Guid {
    data1: 0x5bffa85a,
    data2: 0x3384,
    data3: 0x3540,
    data4: [0x99, 0x40, 0x69, 0x91, 0x20, 0xd4, 0x28, 0xa8],
};

const IID_LANGUAGE_FACTORY: Guid = Guid {
    data1: 0x9b0252ac,
    data2: 0x0c27,
    data3: 0x44f8,
    data4: [0xb7, 0x92, 0x97, 0x93, 0xfb, 0x66, 0xc6, 0x3e],
};

const IID_SOFTWARE_BITMAP_STATICS: Guid = Guid {
    data1: 0xdf0385db,
    data2: 0x672f,
    data3: 0x4a9d,
    data4: [0x80, 0x6e, 0xc2, 0x44, 0x2f, 0x34, 0x3e, 0x86],
};

const IID_BUFFER_FACTORY: Guid = Guid {
    data1: 0x71af914d,
    data2: 0xc10f,
    data3: 0x484b,
    data4: [0xbc, 0x50, 0x14, 0xbc, 0x62, 0x3b, 0x3a, 0x27],
};

const IID_BUFFER_BYTE_ACCESS: Guid = Guid {
    data1: 0x905a0fef,
    data2: 0xbc53,
    data3: 0x11df,
    data4: [0x8c, 0x49, 0x00, 0x1e, 0x4f, 0xc6, 0x86, 0xda],
};

const IID_ASYNC_INFO: Guid = Guid {
    data1: 0x00000036,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

fn check_hr(hr: i32) -> Result<()> {
    if hr < 0 {
        return Err(KaxError::blocked(format!(
            "windows_ocr_error_0x{:08x}",
            hr as u32
        )));
    }
    Ok(())
}

struct ComPtr(*mut c_void);

impl ComPtr {
    fn new(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0
    }

    fn is_null(&self) -> bool {
        self.0.is_null()
    }

    unsafe fn vtable_entry(&self, slot: usize) -> *const c_void {
        let vtable = *(self.0 as *const *const *const c_void);
        *vtable.add(slot)
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let release: extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(self.vtable_entry(2));
                release(self.0);
            }
        }
    }
}

struct HString(*mut c_void);

impl HString {
    fn new(text: &str) -> Result<Self> {
        let api = combase()?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut out: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { (api.windows_create_string)(wide.as_ptr(), wide.len() as u32, &mut out) };
        check_hr(hr)?;
        Ok(Self(out))
    }

    fn take_to_string(ptr: *mut c_void) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let Ok(api) = combase() else {
            return String::new();
        };
        let mut len: u32 = 0;
        let raw = unsafe { (api.windows_get_string_raw_buffer)(ptr, &mut len) };
        let out = if len > 0 && !raw.is_null() {
            let slice = unsafe { std::slice::from_raw_parts(raw, len as usize) };
            OsString::from_wide(slice).to_string_lossy().into_owned()
        } else {
            String::new()
        };
        unsafe {
            (api.windows_delete_string)(ptr);
        }
        out
    }
}

impl Drop for HString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            if let Ok(api) = combase() {
                unsafe {
                    (api.windows_delete_string)(self.0);
                }
            }
        }
    }
}

struct RoGuard(bool);

impl RoGuard {
    fn init() -> Result<Self> {
        let api = combase()?;
        let hr = unsafe { (api.ro_initialize)(1) }; // RO_INIT_MULTITHREADED
        if hr == -2147417850i32 {
            // RPC_E_CHANGED_MODE
            return Ok(Self(false));
        }
        check_hr(hr)?;
        Ok(Self(true))
    }
}

impl Drop for RoGuard {
    fn drop(&mut self) {
        if self.0 {
            if let Ok(api) = combase() {
                unsafe { (api.ro_uninitialize)() };
            }
        }
    }
}

fn get_factory(class_name: &str, iid: &Guid) -> Result<ComPtr> {
    let api = combase()?;
    let h = HString::new(class_name)?;
    let mut out: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { (api.ro_get_activation_factory)(h.0, iid, &mut out) };
    check_hr(hr)?;
    Ok(ComPtr::new(out))
}

unsafe fn query_interface(obj: &ComPtr, iid: &Guid) -> Result<ComPtr> {
    let f: extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        std::mem::transmute(obj.vtable_entry(0));
    let mut out: *mut c_void = std::ptr::null_mut();
    check_hr(f(obj.as_ptr(), iid, &mut out))?;
    Ok(ComPtr::new(out))
}

pub struct WinRtOcr;

impl WinRtOcr {
    pub fn new() -> Self {
        Self
    }

    fn create_engine(language: &str) -> Result<(ComPtr, u32, RoGuard)> {
        let ro = RoGuard::init()?;
        let ocr_factory = get_factory("Windows.Media.Ocr.OcrEngine", &IID_OCR_ENGINE_STATICS)?;
        let lang_factory = get_factory("Windows.Globalization.Language", &IID_LANGUAGE_FACTORY)?;
        let tag = HString::new(language)?;

        // ILanguageFactory::CreateLanguage (slot 6)
        let lang = unsafe {
            let f: extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32 =
                std::mem::transmute(lang_factory.vtable_entry(6));
            let mut out: *mut c_void = std::ptr::null_mut();
            check_hr(f(lang_factory.as_ptr(), tag.0, &mut out))?;
            ComPtr::new(out)
        };

        // IOcrEngineStatics::TryCreateFromLanguage (slot 9)
        let engine = unsafe {
            let f: extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32 =
                std::mem::transmute(ocr_factory.vtable_entry(9));
            let mut out: *mut c_void = std::ptr::null_mut();
            check_hr(f(ocr_factory.as_ptr(), lang.as_ptr(), &mut out))?;
            ComPtr::new(out)
        };
        if engine.is_null() {
            return Err(KaxError::blocked("windows_ocr_language_unavailable"));
        }

        // IOcrEngineStatics::get_MaxImageDimension (slot 6)
        let max_dim = unsafe {
            let f: extern "system" fn(*mut c_void, *mut u32) -> i32 =
                std::mem::transmute(ocr_factory.vtable_entry(6));
            let mut dim: u32 = 0;
            check_hr(f(ocr_factory.as_ptr(), &mut dim))?;
            dim
        };

        // Drop factories and language before returning; keep `ro` LAST so `engine`
        // is always released BEFORE `RoUninitialize()` runs.
        drop(lang);
        drop(tag);
        drop(lang_factory);
        drop(ocr_factory);

        Ok((engine, max_dim, ro))
    }
}

impl Default for WinRtOcr {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrBackend for WinRtOcr {
    fn check_available(&self, language: &str) -> Result<()> {
        let (engine, _max_dim, _ro) = Self::create_engine(language)?;
        drop(engine);
        Ok(())
    }

    fn recognize(&self, language: &str, frame: &Frame) -> Result<Vec<OcrLine>> {
        let (engine, max_dim, _ro) = Self::create_engine(language)?;
        let result = (|| -> Result<Vec<OcrLine>> {
            if frame.width.max(frame.height) > max_dim as usize {
                return Err(KaxError::blocked("windows_ocr_image_too_large"));
            }

            let bitmap_factory = get_factory(
                "Windows.Graphics.Imaging.SoftwareBitmap",
                &IID_SOFTWARE_BITMAP_STATICS,
            )?;
            let buffer_factory = get_factory("Windows.Storage.Streams.Buffer", &IID_BUFFER_FACTORY)?;

            let size = frame.data.len() as u32;
            // IBufferFactory::Create (slot 6)
            let ibuffer = unsafe {
                let f: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
                    std::mem::transmute(buffer_factory.vtable_entry(6));
                let mut out: *mut c_void = std::ptr::null_mut();
                check_hr(f(buffer_factory.as_ptr(), size, &mut out))?;
                ComPtr::new(out)
            };

            // IBufferByteAccess::Buffer (slot 3)
            let access = unsafe { query_interface(&ibuffer, &IID_BUFFER_BYTE_ACCESS)? };
            unsafe {
                let f: extern "system" fn(*mut c_void, *mut *mut u8) -> i32 =
                    std::mem::transmute(access.vtable_entry(3));
                let mut addr: *mut u8 = std::ptr::null_mut();
                check_hr(f(access.as_ptr(), &mut addr))?;
                std::ptr::copy_nonoverlapping(frame.data.as_ptr(), addr, frame.data.len());
            }

            // IBuffer::put_Length (slot 8)
            unsafe {
                let f: extern "system" fn(*mut c_void, u32) -> i32 =
                    std::mem::transmute(ibuffer.vtable_entry(8));
                check_hr(f(ibuffer.as_ptr(), size))?;
            }

            // ISoftwareBitmapStatics::CreateCopyWithAlphaFromBuffer (slot 10): Bgra8 (87), Ignore (2)
            let image = unsafe {
                let f: extern "system" fn(
                    *mut c_void,
                    *mut c_void,
                    i32,
                    i32,
                    i32,
                    i32,
                    *mut *mut c_void,
                ) -> i32 = std::mem::transmute(bitmap_factory.vtable_entry(10));
                let mut out: *mut c_void = std::ptr::null_mut();
                check_hr(f(
                    bitmap_factory.as_ptr(),
                    ibuffer.as_ptr(),
                    87,
                    frame.width as i32,
                    frame.height as i32,
                    2,
                    &mut out,
                ))?;
                ComPtr::new(out)
            };

            // IOcrEngine::RecognizeAsync (slot 6)
            let operation = unsafe {
                let f: extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32 =
                    std::mem::transmute(engine.vtable_entry(6));
                let mut out: *mut c_void = std::ptr::null_mut();
                check_hr(f(engine.as_ptr(), image.as_ptr(), &mut out))?;
                ComPtr::new(out)
            };

            let info = unsafe { query_interface(&operation, &IID_ASYNC_INFO)? };
            // IAsyncInfo::get_Status (slot 7) -> 0 = Started, 1 = Completed, 2 = Canceled, 3 = Error
            let done = until_bool(
                || unsafe {
                    let f: extern "system" fn(*mut c_void, *mut i32) -> i32 =
                        std::mem::transmute(info.vtable_entry(7));
                    let mut st: i32 = 0;
                    f(info.as_ptr(), &mut st) >= 0 && st != 0
                },
                5.0,
                0.01,
            );
            if !done {
                unsafe {
                    let cancel: extern "system" fn(*mut c_void) -> i32 =
                        std::mem::transmute(info.vtable_entry(9));
                    let _ = cancel(info.as_ptr());
                }
                return Err(KaxError::blocked("windows_ocr_timeout"));
            }

            // IAsyncOperation<OcrResult>::GetResults (slot 8)
            let result = unsafe {
                let f: extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
                    std::mem::transmute(operation.vtable_entry(8));
                let mut out: *mut c_void = std::ptr::null_mut();
                check_hr(f(operation.as_ptr(), &mut out))?;
                ComPtr::new(out)
            };

            // IOcrResult::get_Lines (slot 6)
            let lines_vec = unsafe {
                let f: extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
                    std::mem::transmute(result.vtable_entry(6));
                let mut out: *mut c_void = std::ptr::null_mut();
                check_hr(f(result.as_ptr(), &mut out))?;
                ComPtr::new(out)
            };

            // IVectorView<OcrLine>::get_Size (slot 7)
            let line_count = unsafe {
                let f: extern "system" fn(*mut c_void, *mut u32) -> i32 =
                    std::mem::transmute(lines_vec.vtable_entry(7));
                let mut n: u32 = 0;
                check_hr(f(lines_vec.as_ptr(), &mut n))?;
                n
            };

            let mut output = Vec::new();
            for idx in 0..line_count {
                let line = unsafe {
                    let f: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
                        std::mem::transmute(lines_vec.vtable_entry(6));
                    let mut out: *mut c_void = std::ptr::null_mut();
                    check_hr(f(lines_vec.as_ptr(), idx, &mut out))?;
                    ComPtr::new(out)
                };
                let words = unsafe {
                    let f: extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
                        std::mem::transmute(line.vtable_entry(6));
                    let mut out: *mut c_void = std::ptr::null_mut();
                    check_hr(f(line.as_ptr(), &mut out))?;
                    ComPtr::new(out)
                };
                let word_count = unsafe {
                    let f: extern "system" fn(*mut c_void, *mut u32) -> i32 =
                        std::mem::transmute(words.vtable_entry(7));
                    let mut n: u32 = 0;
                    check_hr(f(words.as_ptr(), &mut n))?;
                    n
                };
                let mut bounds = Vec::with_capacity(word_count as usize);
                for j in 0..word_count {
                    let word = unsafe {
                        let f: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
                            std::mem::transmute(words.vtable_entry(6));
                        let mut out: *mut c_void = std::ptr::null_mut();
                        check_hr(f(words.as_ptr(), j, &mut out))?;
                        ComPtr::new(out)
                    };
                    let rect = unsafe {
                        let f: extern "system" fn(*mut c_void, *mut WinRtRect) -> i32 =
                            std::mem::transmute(word.vtable_entry(6));
                        let mut r = WinRtRect::default();
                        check_hr(f(word.as_ptr(), &mut r))?;
                        r
                    };
                    bounds.push(rect);
                }
                if !bounds.is_empty() {
                    let x = bounds
                        .iter()
                        .map(|r| r.x as f64)
                        .fold(f64::INFINITY, f64::min);
                    let y = bounds
                        .iter()
                        .map(|r| r.y as f64)
                        .fold(f64::INFINITY, f64::min);
                    let right = bounds
                        .iter()
                        .map(|r| (r.x + r.width) as f64)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let bottom = bounds
                        .iter()
                        .map(|r| (r.y + r.height) as f64)
                        .fold(f64::NEG_INFINITY, f64::max);

                    let text_ptr = unsafe {
                        let f: extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
                            std::mem::transmute(line.vtable_entry(7));
                        let mut s: *mut c_void = std::ptr::null_mut();
                        check_hr(f(line.as_ptr(), &mut s))?;
                        s
                    };
                    let text = HString::take_to_string(text_ptr);
                    output.push(OcrLine {
                        text,
                        x,
                        y,
                        width: right - x,
                        height: bottom - y,
                    });
                }
            }
            Ok(output)
        })();
        drop(engine);
        result
    }
}
