//! Diagnostic tool: `cargo run --example stream_check -- <url> [relay|segmenter|transcode] [--ua X] [--referer Y]`
//! Prints the probe result; with a mode, serves the stream through the local proxy for 90 seconds.

use fluxo_lib::{net::StreamHeaders, probe, proxy::Proxy, segmenter, transcode};
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(url) = args.next() else {
        eprintln!(
            "usage: stream_check <url> [relay|segmenter|transcode] [--ua X] [--referer Y] [--live]"
        );
        std::process::exit(2);
    };
    let mut mode = None;
    let mut headers = StreamHeaders::default();
    let mut live = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ua" => headers.user_agent = args.next(),
            "--referer" => headers.referrer = args.next(),
            "--live" => live = true,
            other => mode = Some(other.to_owned()),
        }
    }
    let result = probe::probe(&url, &headers);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    let Some(mode) = mode else { return };
    let proxy = Proxy::start(std::env::temp_dir().join("fluxo-stream-check")).unwrap();
    let opened = match mode.as_str() {
        "relay" => proxy.open_relay(&url, headers),
        "segmenter" => {
            let source = if url.starts_with('/') {
                segmenter::Source::File(url.clone().into())
            } else {
                segmenter::Source::Http {
                    url: url.clone(),
                    headers,
                }
            };
            proxy.open_segmenter(source, live)
        }
        _ => proxy.open_transcode(transcode::Options {
            input: &url,
            headers: &headers,
            live,
            copy_video: false,
            deinterlace: false,
        }),
    };
    match opened {
        Ok(opened) => {
            println!("LOCAL {}", opened.url);
            for _ in 0..90 {
                std::thread::sleep(Duration::from_secs(1));
            }
            println!(
                "{}",
                serde_json::to_string(&proxy.status(&opened.session)).unwrap()
            );
        }
        Err(error) => println!("ERROR {error}"),
    }
}
