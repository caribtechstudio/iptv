//! macOS video surface: a `CAOpenGLLayer` into which libmpv renders through its render API.
//!
//! mpv cannot attach to a foreign `NSView` on macOS (`wid` opens a separate window), so the
//! frames are drawn by Fluxo, like IINA does. Core Animation polls `canDrawInCGLContext` at
//! every display refresh; a frame is drawn only when mpv announced one, or when the layer
//! size changed.

use super::{Mpv, ffi};
use objc2::{
    AllocAnyThread, DefinedClass, Encode, Encoding, MainThreadMarker, MainThreadOnly, Message,
    define_class, extern_class, msg_send,
    rc::Retained,
    runtime::{Bool, NSObject},
};
use objc2_app_kit::{NSView, NSWindowOrderingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::{
    ffi::{c_char, c_void},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

// ---------- OpenGL / CGL ----------

#[repr(transparent)]
#[derive(Clone, Copy)]
struct PixelFormat(*mut c_void);
// SAFETY: matches `CGLPixelFormatObj` (`struct _CGLPixelFormatObject *`).
unsafe impl Encode for PixelFormat {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("_CGLPixelFormatObject", &[]));
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct GlContext(*mut c_void);
// SAFETY: matches `CGLContextObj` (`struct _CGLContextObject *`).
unsafe impl Encode for GlContext {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("_CGLContextObject", &[]));
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct TimeStamp(*const c_void);
// SAFETY: matches `const CVTimeStamp *`; the structure is never read.
unsafe impl Encode for TimeStamp {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct(
        "CVTimeStamp",
        &[
            Encoding::UInt,
            Encoding::Int,
            Encoding::LongLong,
            Encoding::ULongLong,
            Encoding::Double,
            Encoding::LongLong,
            Encoding::Struct(
                "CVSMPTETime",
                &[
                    Encoding::Short,
                    Encoding::Short,
                    Encoding::UInt,
                    Encoding::UInt,
                    Encoding::UInt,
                    Encoding::Short,
                    Encoding::Short,
                    Encoding::Short,
                    Encoding::Short,
                ],
            ),
            Encoding::ULongLong,
            Encoding::ULongLong,
        ],
    ));
}

const CGL_PFA_DOUBLE_BUFFER: i32 = 5;
const CGL_PFA_COLOR_SIZE: i32 = 8;
const CGL_PFA_ALPHA_SIZE: i32 = 11;
const CGL_PFA_ACCELERATED: i32 = 73;
const CGL_PFA_OPENGL_PROFILE: i32 = 99;
const CGL_PFA_AUTOMATIC_GRAPHICS_SWITCHING: i32 = 101;
const CGL_OGLP_VERSION_3_2_CORE: i32 = 0x3200;

const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;

#[link(name = "OpenGL", kind = "framework")]
unsafe extern "C" {
    fn CGLChoosePixelFormat(attribs: *const i32, pix: *mut *mut c_void, npix: *mut i32) -> i32;
    fn CGLCreateContext(pix: *mut c_void, share: *mut c_void, ctx: *mut *mut c_void) -> i32;
    fn CGLRetainContext(ctx: *mut c_void) -> *mut c_void;
    fn CGLReleaseContext(ctx: *mut c_void);
    fn CGLRetainPixelFormat(pix: *mut c_void) -> *mut c_void;
    fn CGLReleasePixelFormat(pix: *mut c_void);
    fn CGLSetCurrentContext(ctx: *mut c_void) -> i32;
    fn CGLGetCurrentContext() -> *mut c_void;
    fn glGetIntegerv(name: u32, data: *mut i32);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: u32);
    fn glReadPixels(x: i32, y: i32, w: i32, h: i32, format: u32, kind: u32, data: *mut c_void);
    fn glBindFramebuffer(target: u32, framebuffer: u32);
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `RTLD_DEFAULT` on macOS.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    unsafe { dlsym(RTLD_DEFAULT, name) }
}

// ---------- Shared state ----------

struct RenderHandle {
    context: *mut ffi::RenderContext,
}
// SAFETY: the render context is only used under `Shared::render`'s lock, with its GL context
// made current first, as libmpv's render API requires.
unsafe impl Send for RenderHandle {}

/// Counters readable from any thread, used by diagnostics.
#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    /// Centre pixel of the last frame as `0xRRGGBBAA`, when pixel sampling is enabled.
    pub centre: AtomicU32,
}

pub struct Shared {
    api: &'static ffi::Api,
    /// Released when the surface closes, so that the (possibly slow) destruction of mpv never
    /// happens on the main thread, whenever Core Animation frees the layer.
    mpv: Mutex<Option<Arc<Mpv>>>,
    /// OpenGL pixel format and context owned by the surface (`CGLPixelFormatObj`,
    /// `CGLContextObj`), handed to Core Animation with an extra reference.
    pixel_format: usize,
    gl: usize,
    render: Mutex<Option<RenderHandle>>,
    needs_frame: AtomicBool,
    drawn_size: Mutex<(i32, i32)>,
    closed: AtomicBool,
    sample_pixels: bool,
    pub stats: Stats,
}

unsafe extern "C" fn on_update(data: *mut c_void) {
    // SAFETY: `data` is the `Shared` kept alive by the layer until the callback is removed.
    let shared = unsafe { &*data.cast::<Shared>() };
    shared.needs_frame.store(true, Ordering::Release);
}

impl Shared {
    /// Creates the OpenGL context and connects mpv to it. It must exist before a file is
    /// loaded: mpv disables video for the whole file when no render context is set.
    fn create_render_context(&self, mpv: &Mpv) -> Result<(), String> {
        let api = self.api;
        let mut init = ffi::OpenGlInitParams {
            get_proc_address,
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut params = [
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_API_TYPE,
                data: c"opengl".as_ptr().cast_mut().cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&raw mut init).cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let mut context = ptr::null_mut();
        let mut render = self.render.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            let previous = CGLGetCurrentContext();
            CGLSetCurrentContext(self.gl as *mut c_void);
            let code = (api.render_context_create)(&mut context, mpv.raw(), params.as_mut_ptr());
            CGLSetCurrentContext(previous);
            if code < 0 || context.is_null() {
                return Err(format!(
                    "Le rendu vidéo de mpv n’a pas pu démarrer : {}",
                    mpv.error_text(code)
                ));
            }
            (api.render_context_set_update_callback)(
                context,
                Some(on_update),
                ptr::from_ref(self).cast_mut().cast(),
            );
        }
        *render = Some(RenderHandle { context });
        Ok(())
    }

    fn can_draw(&self, width: i32, height: i32) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let resized = *self.drawn_size.lock().unwrap_or_else(|e| e.into_inner()) != (width, height);
        if self.needs_frame.swap(false, Ordering::AcqRel) {
            let render = self.render.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(handle) = render.as_ref() {
                let flags = unsafe { (self.api.render_context_update)(handle.context) };
                if flags & ffi::RENDER_UPDATE_FRAME != 0 {
                    return true;
                }
            }
        }
        resized
    }

    fn draw(&self) {
        let render = self.render.lock().unwrap_or_else(|e| e.into_inner());
        let mut viewport = [0i32; 4];
        let mut fbo = 0i32;
        unsafe {
            glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr());
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut fbo);
        }
        let (width, height) = (viewport[2], viewport[3]);
        *self.drawn_size.lock().unwrap_or_else(|e| e.into_inner()) = (width, height);
        let Some(handle) = render
            .as_ref()
            .filter(|_| !self.closed.load(Ordering::Acquire))
        else {
            unsafe {
                glClearColor(0.0, 0.0, 0.0, 1.0);
                glClear(GL_COLOR_BUFFER_BIT);
            }
            return;
        };
        let mut target = ffi::OpenGlFbo {
            fbo,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: i32 = 1;
        // Never block the drawing thread waiting for the frame's display time.
        let mut block: i32 = 0;
        let mut params = [
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_OPENGL_FBO,
                data: (&raw mut target).cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_FLIP_Y,
                data: (&raw mut flip).cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: (&raw mut block).cast(),
            },
            ffi::RenderParam {
                kind: ffi::RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        unsafe { (self.api.render_context_render)(handle.context, params.as_mut_ptr()) };
        let frames = self.stats.frames.fetch_add(1, Ordering::Relaxed) + 1;
        if self.sample_pixels && width > 0 && height > 0 {
            let mut pixel = [0u8; 4];
            unsafe {
                glBindFramebuffer(GL_READ_FRAMEBUFFER, fbo as u32);
                glReadPixels(
                    width / 2,
                    height / 2,
                    1,
                    1,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    pixel.as_mut_ptr().cast(),
                );
            }
            let centre = u32::from_be_bytes(pixel);
            self.stats.centre.store(centre, Ordering::Relaxed);
            if frames.is_multiple_of(120) {
                eprintln!("[mpv] {frames} images · {width}×{height} px · centre #{centre:08x}");
            }
        }
    }

    fn after_swap(&self) {
        let render = self.render.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = render.as_ref() {
            unsafe { (self.api.render_context_report_swap)(handle.context) };
        }
    }

    /// Frees the render context. Must happen before the mpv instance is destroyed.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let mut render = self.render.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = render.take() {
            let api = self.api;
            unsafe {
                let previous = CGLGetCurrentContext();
                CGLSetCurrentContext(self.gl as *mut c_void);
                (api.render_context_set_update_callback)(handle.context, None, ptr::null_mut());
                (api.render_context_free)(handle.context);
                CGLSetCurrentContext(previous);
            }
        }
        drop(render);
        self.mpv.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.close();
        unsafe {
            CGLReleaseContext(self.gl as *mut c_void);
            CGLReleasePixelFormat(self.pixel_format as *mut c_void);
        }
    }
}

/// Pixel format and context for mpv's renderer: OpenGL 3.2 core, hardware accelerated.
fn create_gl() -> Result<(usize, usize), String> {
    let attributes = [
        CGL_PFA_OPENGL_PROFILE,
        CGL_OGLP_VERSION_3_2_CORE,
        CGL_PFA_ACCELERATED,
        CGL_PFA_DOUBLE_BUFFER,
        CGL_PFA_COLOR_SIZE,
        24,
        CGL_PFA_ALPHA_SIZE,
        8,
        CGL_PFA_AUTOMATIC_GRAPHICS_SWITCHING,
        0,
    ];
    let mut format = ptr::null_mut();
    let mut count = 0;
    unsafe { CGLChoosePixelFormat(attributes.as_ptr(), &mut format, &mut count) };
    if format.is_null() {
        return Err("OpenGL n’est pas disponible pour le rendu vidéo de mpv.".into());
    }
    let mut context = ptr::null_mut();
    unsafe { CGLCreateContext(format, ptr::null_mut(), &mut context) };
    if context.is_null() {
        unsafe { CGLReleasePixelFormat(format) };
        return Err("Contexte OpenGL impossible à créer pour mpv.".into());
    }
    Ok((format as usize, context as usize))
}

// ---------- Layer class ----------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "CALayer"]
    struct CALayer;
);

extern_class!(
    #[unsafe(super(CALayer, NSObject))]
    #[name = "CAOpenGLLayer"]
    struct CAOpenGLLayer;
);

pub struct Ivars {
    shared: Arc<Shared>,
}

define_class!(
    #[unsafe(super(CAOpenGLLayer, CALayer, NSObject))]
    #[name = "FluxoMpvLayer"]
    #[ivars = Ivars]
    struct MpvLayer;

    impl MpvLayer {
        #[unsafe(method(copyCGLPixelFormatForDisplayMask:))]
        fn copy_pixel_format(&self, _mask: u32) -> PixelFormat {
            let shared = &self.ivars().shared;
            PixelFormat(unsafe { CGLRetainPixelFormat(shared.pixel_format as *mut c_void) })
        }

        #[unsafe(method(copyCGLContextForPixelFormat:))]
        fn copy_context(&self, _format: PixelFormat) -> GlContext {
            GlContext(unsafe { CGLRetainContext(self.ivars().shared.gl as *mut c_void) })
        }

        #[unsafe(method(canDrawInCGLContext:pixelFormat:forLayerTime:displayTime:))]
        fn can_draw(
            &self,
            _context: GlContext,
            _format: PixelFormat,
            _time: f64,
            _stamp: TimeStamp,
        ) -> Bool {
            let (width, height) = self.pixel_size();
            Bool::new(self.ivars().shared.can_draw(width, height))
        }

        #[unsafe(method(drawInCGLContext:pixelFormat:forLayerTime:displayTime:))]
        fn draw(&self, context: GlContext, format: PixelFormat, time: f64, stamp: TimeStamp) {
            self.ivars().shared.draw();
            // The superclass flushes the frame to the screen.
            let _: () = unsafe {
                msg_send![super(self), drawInCGLContext: context, pixelFormat: format, forLayerTime: time, displayTime: stamp]
            };
            self.ivars().shared.after_swap();
        }
    }
);

impl MpvLayer {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { shared });
        let layer: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            let _: () = msg_send![&*layer, setAsynchronous: true];
            let _: () = msg_send![&*layer, setNeedsDisplayOnBoundsChange: true];
            let _: () = msg_send![&*layer, setOpaque: true];
        }
        layer
    }

    fn pixel_size(&self) -> (i32, i32) {
        let bounds: NSRect = unsafe { msg_send![self, bounds] };
        let scale: f64 = unsafe { msg_send![self, contentsScale] };
        (
            (bounds.size.width * scale).round() as i32,
            (bounds.size.height * scale).round() as i32,
        )
    }
}

// ---------- Surface ----------

/// Position of the video, in points, in the coordinates of the surface's reference view
/// (the web view: origin at its top-left corner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A native view showing one mpv instance. Main thread only.
pub struct Surface {
    view: Retained<NSView>,
    reference: Retained<NSView>,
    layer: Retained<MpvLayer>,
    shared: Arc<Shared>,
}

impl Surface {
    /// Creates the view and inserts it just below `reference` (the web view) in the same
    /// parent, so the HTML controls stay on top of the video. Frames are then given in the
    /// coordinates of `reference`.
    pub fn new(mtm: MainThreadMarker, mpv: Arc<Mpv>, reference: &NSView) -> Result<Self, String> {
        let parent = unsafe { reference.superview() }
            .ok_or("La vue web n’est pas encore dans une fenêtre.")?;
        let (pixel_format, gl) = create_gl()?;
        let shared = Arc::new(Shared {
            api: mpv.api(),
            mpv: Mutex::new(Some(mpv.clone())),
            pixel_format,
            gl,
            render: Mutex::new(None),
            needs_frame: AtomicBool::new(true),
            drawn_size: Mutex::new((0, 0)),
            closed: AtomicBool::new(false),
            sample_pixels: std::env::var_os("FLUXO_MPV_SAMPLE").is_some(),
            stats: Stats::default(),
        });
        shared.create_render_context(&mpv)?;
        let layer = MpvLayer::new(shared.clone());
        let view = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
        );
        unsafe {
            let _: () = msg_send![&*view, setLayer: &*layer];
        }
        view.setWantsLayer(true);
        view.setHidden(true);
        parent.addSubview_positioned_relativeTo(
            &view,
            NSWindowOrderingMode::Below,
            Some(reference),
        );
        Ok(Self {
            view,
            reference: reference.retain(),
            layer,
            shared,
        })
    }

    pub fn stats(&self) -> &Stats {
        &self.shared.stats
    }

    /// Moves the view; `None` hides it.
    pub fn set_frame(&self, frame: Option<Frame>) {
        let Some(frame) = frame else {
            self.view.setHidden(true);
            return;
        };
        let Some(parent) = (unsafe { self.view.superview() }) else {
            return;
        };
        let rect = NSRect::new(
            NSPoint::new(frame.x, frame.y),
            NSSize::new(frame.width, frame.height),
        );
        // Handles the web view being flipped (top-left origin) while its parent is not.
        self.view
            .setFrame(parent.convertRect_fromView(rect, Some(&self.reference)));
        if let Some(window) = self.view.window() {
            let scale = window.backingScaleFactor();
            unsafe {
                let _: () = msg_send![&*self.layer, setContentsScale: scale];
            }
        }
        self.view.setHidden(false);
        unsafe {
            let _: () = msg_send![&*self.layer, setNeedsDisplay];
        }
    }

    /// Frees the render context and removes the view. mpv can be destroyed afterwards.
    pub fn close(&self) {
        self.shared.close();
        self.view.removeFromSuperview();
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.close();
    }
}
