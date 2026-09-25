//! Safe wrapper around one libmpv instance.
//!
//! libmpv functions are thread-safe; only `wait_event` must be called from a single thread,
//! which `Mpv::wait_event` enforces by taking `&mut self` through the event loop owner.

pub mod ffi;
#[cfg(target_os = "macos")]
pub mod macos;

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr::NonNull,
};

/// Value of an observed property.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    None,
    Flag(bool),
    Int(i64),
    Double(f64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    None,
    Shutdown,
    StartFile,
    FileLoaded,
    VideoReconfig,
    PlaybackRestart,
    /// Playback of the current file stopped. `error` is set when it failed.
    EndFile {
        eof: bool,
        error: Option<String>,
    },
    Property {
        name: String,
        value: Value,
    },
    Log {
        level: String,
        prefix: String,
        text: String,
    },
    Other,
}

/// Format in which a property is observed.
#[derive(Clone, Copy, Debug)]
pub enum Format {
    Flag,
    Int,
    Double,
    Text,
}

impl Format {
    fn raw(self) -> c_int {
        match self {
            Self::Flag => ffi::FORMAT_FLAG,
            Self::Int => ffi::FORMAT_INT64,
            Self::Double => ffi::FORMAT_DOUBLE,
            Self::Text => ffi::FORMAT_STRING,
        }
    }
}

pub struct Mpv {
    api: &'static ffi::Api,
    handle: NonNull<ffi::Handle>,
}

// SAFETY: the libmpv client API is thread-safe (see `mpv/client.h`, "Threading").
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

fn c_string(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "Valeur invalide pour mpv (octet nul).".to_owned())
}

/// # Safety
/// `pointer` must be null or a valid NUL-terminated string.
unsafe fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Whether libmpv could be loaded, and from where.
pub fn available() -> Result<String, String> {
    ffi::api().map(|api| api.path.to_string_lossy().into_owned())
}

impl Mpv {
    /// Creates an instance, applies `options` (set before initialisation, as some options such
    /// as `wid` or `vo` require) and initialises it.
    pub fn new(options: &[(&str, &str)]) -> Result<Self, String> {
        let api = ffi::api()?;
        let handle = NonNull::new(unsafe { (api.create)() })
            .ok_or("mpv n’a pas pu être créé (mémoire insuffisante).")?;
        let mpv = Self { api, handle };
        for (name, value) in options {
            let name_c = c_string(name)?;
            let value_c = c_string(value)?;
            let code =
                unsafe { (api.set_option_string)(mpv.raw(), name_c.as_ptr(), value_c.as_ptr()) };
            mpv.check(code)
                .map_err(|error| format!("Option mpv « {name}={value} » refusée : {error}"))?;
        }
        mpv.check(unsafe { (api.initialize)(mpv.raw()) })
            .map_err(|error| format!("Initialisation de mpv impossible : {error}"))?;
        Ok(mpv)
    }

    pub(crate) fn raw(&self) -> *mut ffi::Handle {
        self.handle.as_ptr()
    }

    pub(crate) fn api(&self) -> &'static ffi::Api {
        self.api
    }

    pub(crate) fn error_text(&self, code: c_int) -> String {
        unsafe { text((self.api.error_string)(code)) }
    }

    fn check(&self, code: c_int) -> Result<(), String> {
        if code >= 0 {
            Ok(())
        } else {
            Err(self.error_text(code))
        }
    }

    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned = args
            .iter()
            .map(|arg| c_string(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let mut pointers: Vec<*const c_char> = owned.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(std::ptr::null());
        self.check(unsafe { (self.api.command)(self.raw(), pointers.as_mut_ptr()) })
            .map_err(|error| format!("Commande mpv « {} » refusée : {error}", args.join(" ")))
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<(), String> {
        let name_c = c_string(name)?;
        let value_c = c_string(value)?;
        self.check(unsafe {
            (self.api.set_property_string)(self.raw(), name_c.as_ptr(), value_c.as_ptr())
        })
        .map_err(|error| format!("Propriété mpv « {name} » refusée : {error}"))
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<(), String> {
        let name_c = c_string(name)?;
        let mut flag: c_int = value.into();
        self.check(unsafe {
            (self.api.set_property)(
                self.raw(),
                name_c.as_ptr(),
                ffi::FORMAT_FLAG,
                (&raw mut flag).cast(),
            )
        })
        .map_err(|error| format!("Propriété mpv « {name} » refusée : {error}"))
    }

    pub fn set_double(&self, name: &str, value: f64) -> Result<(), String> {
        let name_c = c_string(name)?;
        let mut number = value;
        self.check(unsafe {
            (self.api.set_property)(
                self.raw(),
                name_c.as_ptr(),
                ffi::FORMAT_DOUBLE,
                (&raw mut number).cast(),
            )
        })
        .map_err(|error| format!("Propriété mpv « {name} » refusée : {error}"))
    }

    pub fn get_double(&self, name: &str) -> Option<f64> {
        let name_c = c_string(name).ok()?;
        let mut number = 0.0f64;
        let code = unsafe {
            (self.api.get_property)(
                self.raw(),
                name_c.as_ptr(),
                ffi::FORMAT_DOUBLE,
                (&raw mut number).cast(),
            )
        };
        (code >= 0).then_some(number)
    }

    /// String form of a property; structured properties (`track-list`…) come back as JSON.
    pub fn get_string(&self, name: &str) -> Option<String> {
        let name_c = c_string(name).ok()?;
        let pointer = unsafe { (self.api.get_property_string)(self.raw(), name_c.as_ptr()) };
        if pointer.is_null() {
            return None;
        }
        let value = unsafe { text(pointer) };
        unsafe { (self.api.free)(pointer.cast()) };
        Some(value)
    }

    pub fn observe(&self, name: &str, format: Format) -> Result<(), String> {
        let name_c = c_string(name)?;
        self.check(unsafe {
            (self.api.observe_property)(self.raw(), 0, name_c.as_ptr(), format.raw())
        })
    }

    pub fn request_log_messages(&self, level: &str) -> Result<(), String> {
        let level_c = c_string(level)?;
        self.check(unsafe { (self.api.request_log_messages)(self.raw(), level_c.as_ptr()) })
    }

    /// Interrupts a pending `wait_event`.
    pub fn wakeup(&self) {
        unsafe { (self.api.wakeup)(self.raw()) }
    }

    /// Waits up to `timeout` seconds (negative: forever) for the next event.
    /// Must only be called from one thread at a time.
    pub fn wait_event(&self, timeout: f64) -> Event {
        let raw = unsafe { (self.api.wait_event)(self.raw(), timeout) };
        // SAFETY: libmpv returns a pointer valid until the next `wait_event` call; everything
        // is copied into owned Rust values before returning.
        let Some(event) = (unsafe { raw.as_ref() }) else {
            return Event::None;
        };
        match event.event_id {
            0 => Event::None,
            ffi::EVENT_SHUTDOWN => Event::Shutdown,
            ffi::EVENT_START_FILE => Event::StartFile,
            ffi::EVENT_FILE_LOADED => Event::FileLoaded,
            ffi::EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
            ffi::EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
            ffi::EVENT_END_FILE => {
                let Some(data) = (unsafe { event.data.cast::<ffi::EventEndFile>().as_ref() })
                else {
                    return Event::Other;
                };
                Event::EndFile {
                    eof: data.reason == ffi::END_FILE_EOF,
                    error: (data.reason == ffi::END_FILE_ERROR)
                        .then(|| self.error_text(data.error)),
                }
            }
            ffi::EVENT_PROPERTY_CHANGE => {
                let Some(data) = (unsafe { event.data.cast::<ffi::EventProperty>().as_ref() })
                else {
                    return Event::Other;
                };
                Event::Property {
                    name: unsafe { text(data.name) },
                    value: unsafe { read_value(data.format, data.data) },
                }
            }
            ffi::EVENT_LOG_MESSAGE => {
                let Some(data) = (unsafe { event.data.cast::<ffi::EventLogMessage>().as_ref() })
                else {
                    return Event::Other;
                };
                Event::Log {
                    level: unsafe { text(data.level) },
                    prefix: unsafe { text(data.prefix) },
                    text: unsafe { text(data.text) }.trim_end().to_owned(),
                }
            }
            _ => Event::Other,
        }
    }
}

/// # Safety
/// `data` must point to a value of the given mpv format, or be null.
unsafe fn read_value(format: c_int, data: *mut c_void) -> Value {
    if data.is_null() {
        return Value::None;
    }
    unsafe {
        match format {
            ffi::FORMAT_FLAG => Value::Flag(*data.cast::<c_int>() != 0),
            ffi::FORMAT_INT64 => Value::Int(*data.cast::<i64>()),
            ffi::FORMAT_DOUBLE => Value::Double(*data.cast::<f64>()),
            ffi::FORMAT_STRING => Value::Text(text(*data.cast::<*const c_char>())),
            ffi::FORMAT_NONE => Value::None,
            _ => Value::None,
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.raw()) }
    }
}
