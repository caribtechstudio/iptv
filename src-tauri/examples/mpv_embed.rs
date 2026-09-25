//! Diagnostic: `FLUXO_MPV_SAMPLE=1 cargo run -p fluxo --example mpv_embed -- <url> [seconds]`
//! Opens a window, renders libmpv into Fluxo's video surface and prints how many frames were
//! drawn and the colour of the centre pixel.

#[cfg(target_os = "macos")]
fn main() {
    use fluxo_lib::mpv::{
        Event, Format, Mpv,
        macos::{Frame, Surface},
    };
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSView,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::time::Duration;

    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: mpv_embed <url> [seconds]");
    let seconds: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(8);

    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(900.0, 560.0)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setBackgroundColor(Some(&NSColor::darkGrayColor()));
    let content = window.contentView().expect("content view");
    window.makeKeyAndOrderFront(None);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    let mpv = Mpv::new(&[
        ("vo", "libmpv"),
        ("hwdec", "auto-safe"),
        ("terminal", "no"),
        ("osc", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("keep-open", "yes"),
    ])
    .expect("mpv");
    mpv.observe("time-pos", Format::Double).unwrap();
    mpv.observe("hwdec-current", Format::Text).unwrap();
    mpv.observe("video-codec", Format::Text).unwrap();
    mpv.request_log_messages("warn").unwrap();
    let mpv = std::sync::Arc::new(mpv);
    // Stands in for the web view: the surface is placed below it, in its coordinates.
    let reference = NSView::initWithFrame(NSView::alloc(mtm), content.bounds());
    content.addSubview(&reference);
    let surface = Surface::new(mtm, mpv.clone(), &reference).expect("surface");
    mpv.command(&["loadfile", &url]).unwrap();
    surface.set_frame(Some(Frame {
        x: 60.0,
        y: 60.0,
        width: 640.0,
        height: 360.0,
    }));
    let surface: &'static Surface = Box::leak(Box::new(surface));
    let stats = surface.stats() as *const fluxo_lib::mpv::macos::Stats as usize;
    let events = mpv.clone();
    std::thread::spawn(move || {
        loop {
            match events.wait_event(1.0) {
                Event::None => {}
                Event::Property { name, value } if name != "time-pos" => {
                    println!("{name} = {value:?}")
                }
                Event::Property { .. } => {}
                Event::Shutdown => break,
                other => println!("{other:?}"),
            }
        }
    });
    std::thread::spawn(move || {
        // SAFETY: the surface is leaked, its statistics live until the process exits.
        let stats = unsafe { &*(stats as *const fluxo_lib::mpv::macos::Stats) };
        for _ in 0..seconds {
            std::thread::sleep(Duration::from_secs(1));
            println!(
                "time-pos = {:?} · images dessinées = {} · pixel central = #{:08x}",
                mpv.get_double("time-pos"),
                stats.frames.load(std::sync::atomic::Ordering::Relaxed),
                stats.centre.load(std::sync::atomic::Ordering::Relaxed)
            );
        }
        std::process::exit(0);
    });
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mpv_embed only runs on macOS.");
}
