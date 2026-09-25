//! Minimal libmpv client API (`mpv/client.h`, API 2.x), loaded at run time.
//!
//! Loading the library dynamically keeps Fluxo usable without mpv: the engine is simply
//! reported as unavailable and playback stays on the system player.

use libloading::Library;
use std::{
    ffi::{c_char, c_double, c_int, c_void},
    path::PathBuf,
    sync::OnceLock,
};

#[repr(C)]
pub struct Handle {
    _private: [u8; 0],
}

pub const FORMAT_NONE: c_int = 0;
pub const FORMAT_STRING: c_int = 1;
pub const FORMAT_FLAG: c_int = 3;
pub const FORMAT_INT64: c_int = 4;
pub const FORMAT_DOUBLE: c_int = 5;

pub const EVENT_SHUTDOWN: c_int = 1;
pub const EVENT_LOG_MESSAGE: c_int = 2;
pub const EVENT_START_FILE: c_int = 6;
pub const EVENT_END_FILE: c_int = 7;
pub const EVENT_FILE_LOADED: c_int = 8;
pub const EVENT_VIDEO_RECONFIG: c_int = 17;
pub const EVENT_PLAYBACK_RESTART: c_int = 21;
pub const EVENT_PROPERTY_CHANGE: c_int = 22;

pub const END_FILE_EOF: c_int = 0;
pub const END_FILE_ERROR: c_int = 4;

#[repr(C)]
pub struct Event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct EventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct EventEndFile {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

#[repr(C)]
pub struct EventLogMessage {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

#[repr(C)]
pub struct RenderContext {
    _private: [u8; 0],
}

pub const RENDER_PARAM_INVALID: c_int = 0;
pub const RENDER_PARAM_API_TYPE: c_int = 1;
pub const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const RENDER_PARAM_FLIP_Y: c_int = 4;
pub const RENDER_PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
pub const RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub struct RenderParam {
    pub kind: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct OpenGlInitParams {
    pub get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct OpenGlFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

/// Function table of the loaded library. The `Library` is kept alive for the whole process.
pub struct Api {
    _library: Library,
    pub path: PathBuf,
    pub client_api_version: unsafe extern "C" fn() -> std::ffi::c_ulong,
    pub error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    pub free: unsafe extern "C" fn(*mut c_void),
    pub create: unsafe extern "C" fn() -> *mut Handle,
    pub initialize: unsafe extern "C" fn(*mut Handle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(*mut Handle),
    pub set_option_string: unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    pub set_property: unsafe extern "C" fn(*mut Handle, *const c_char, c_int, *mut c_void) -> c_int,
    pub set_property_string:
        unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    pub get_property: unsafe extern "C" fn(*mut Handle, *const c_char, c_int, *mut c_void) -> c_int,
    pub get_property_string: unsafe extern "C" fn(*mut Handle, *const c_char) -> *mut c_char,
    pub command: unsafe extern "C" fn(*mut Handle, *mut *const c_char) -> c_int,
    pub observe_property: unsafe extern "C" fn(*mut Handle, u64, *const c_char, c_int) -> c_int,
    pub request_log_messages: unsafe extern "C" fn(*mut Handle, *const c_char) -> c_int,
    pub wait_event: unsafe extern "C" fn(*mut Handle, c_double) -> *mut Event,
    pub wakeup: unsafe extern "C" fn(*mut Handle),
    pub render_context_create:
        unsafe extern "C" fn(*mut *mut RenderContext, *mut Handle, *mut RenderParam) -> c_int,
    pub render_context_set_update_callback: unsafe extern "C" fn(
        *mut RenderContext,
        Option<unsafe extern "C" fn(*mut c_void)>,
        *mut c_void,
    ),
    pub render_context_update: unsafe extern "C" fn(*mut RenderContext) -> u64,
    pub render_context_render: unsafe extern "C" fn(*mut RenderContext, *mut RenderParam) -> c_int,
    pub render_context_report_swap: unsafe extern "C" fn(*mut RenderContext),
    pub render_context_free: unsafe extern "C" fn(*mut RenderContext),
}

/// Places where libmpv may live: next to Fluxo (bundled), then the usual install locations.
fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let names: &[&str] = if cfg!(target_os = "macos") {
        &["libmpv.2.dylib", "libmpv.dylib"]
    } else if cfg!(windows) {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else {
        &["libmpv.so.2", "libmpv.so"]
    };
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for name in names {
            paths.push(dir.join("../Resources/native/lib").join(name));
            paths.push(dir.join("native/lib").join(name));
            paths.push(dir.join(name));
        }
    }
    if cfg!(target_os = "macos") {
        for dir in ["/opt/homebrew/lib", "/usr/local/lib", "/opt/local/lib"] {
            paths.extend(names.iter().map(|name| PathBuf::from(dir).join(name)));
        }
    }
    // Last resort: the system loader search path (Linux distributions, Windows PATH).
    paths.extend(names.iter().map(PathBuf::from));
    paths
}

macro_rules! symbol {
    ($library:expr, $name:literal) => {
        // SAFETY: the signature matches `mpv/client.h` for API 2.x, checked below.
        *unsafe { $library.get(concat!($name, "\0").as_bytes()) }
            .map_err(|error| format!("libmpv incomplète ({}) : {error}", $name))?
    };
}

fn load() -> Result<Api, String> {
    let mut last_error = String::from("libmpv introuvable");
    for path in candidates() {
        if path.is_absolute() && !path.is_file() {
            continue;
        }
        // SAFETY: loading libmpv runs its initialisers, which have no preconditions.
        let library = match unsafe { Library::new(&path) } {
            Ok(library) => library,
            Err(error) => {
                last_error = error.to_string();
                continue;
            }
        };
        let api = Api {
            client_api_version: symbol!(library, "mpv_client_api_version"),
            error_string: symbol!(library, "mpv_error_string"),
            free: symbol!(library, "mpv_free"),
            create: symbol!(library, "mpv_create"),
            initialize: symbol!(library, "mpv_initialize"),
            terminate_destroy: symbol!(library, "mpv_terminate_destroy"),
            set_option_string: symbol!(library, "mpv_set_option_string"),
            set_property: symbol!(library, "mpv_set_property"),
            set_property_string: symbol!(library, "mpv_set_property_string"),
            get_property: symbol!(library, "mpv_get_property"),
            get_property_string: symbol!(library, "mpv_get_property_string"),
            command: symbol!(library, "mpv_command"),
            observe_property: symbol!(library, "mpv_observe_property"),
            request_log_messages: symbol!(library, "mpv_request_log_messages"),
            wait_event: symbol!(library, "mpv_wait_event"),
            wakeup: symbol!(library, "mpv_wakeup"),
            render_context_create: symbol!(library, "mpv_render_context_create"),
            render_context_set_update_callback: symbol!(
                library,
                "mpv_render_context_set_update_callback"
            ),
            render_context_update: symbol!(library, "mpv_render_context_update"),
            render_context_render: symbol!(library, "mpv_render_context_render"),
            render_context_report_swap: symbol!(library, "mpv_render_context_report_swap"),
            render_context_free: symbol!(library, "mpv_render_context_free"),
            path,
            _library: library,
        };
        let version = unsafe { (api.client_api_version)() };
        if version >> 16 != 2 {
            last_error = format!("version de libmpv non prise en charge ({})", version >> 16);
            continue;
        }
        return Ok(api);
    }
    Err(last_error)
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

pub fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}
