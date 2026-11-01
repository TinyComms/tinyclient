//! A logged check of the call platform on a phone, started from a debug launch
//! (`am start ... --es tiny.selftest codec`): no taps, nothing shown, every step in logcat under
//! `tinyclient`.
//!
//! `codec`: the registered platform's H.264 encoder at the camera's two layer sizes and bitrates,
//! fed moving NV12 pictures (keyframes at 0 and 30), each access unit's NAL types logged, then
//! the platform's decoder fed the 640×360 stream and the pictures it returns counted.

use mediad::platform::{EncoderConfig, PixelFormat, VideoFrame};
use std::time::{Duration, Instant};

pub fn run(what: &str) {
    let platform = {
        let waited = Instant::now();
        loop {
            if let Some(platform) = mediad::platform::get() {
                break platform;
            }
            if waited.elapsed() > Duration::from_secs(10) {
                log::error!("tinyclient: selftest: no platform registered");
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    match what {
        "codec" => codec(&*platform),
        other => log::warn!("tinyclient: selftest: no test named {other:?}"),
    }
}

/// A moving picture: a diagonal ramp that slides a few pixels a frame, colour bands in CbCr.
fn picture(width: u32, height: u32, n: u32) -> VideoFrame {
    let (w, h) = (width as usize, height as usize);
    let mut data = vec![0u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            data[y * w + x] = ((x + y + n as usize * 4) % 256) as u8;
        }
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let at = w * h + y * w + 2 * x;
            data[at] = (64 + (x * 2 + n as usize) % 128) as u8;
            data[at + 1] = (64 + (y * 2) % 128) as u8;
        }
    }
    VideoFrame { width, height, format: PixelFormat::Nv12, data, timestamp_us: n as u64 * 66_666 }
}

/// The NAL unit types in an Annex B buffer.
fn nal_types(annex_b: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            types.push(annex_b[i + 3] & 0x1f);
            i += 3;
        }
        i += 1;
    }
    types
}

fn codec(platform: &dyn mediad::platform::Platform) {
    log::info!("tinyclient: selftest codec: start");
    let layers = [(640u32, 360u32, 500_000u32), (320, 180, 150_000)];
    let mut high_stream: Vec<Vec<u8>> = Vec::new();
    for (width, height, bitrate) in layers {
        let mut encoder = match platform.h264_encoder(EncoderConfig { width, height, bitrate_bps: bitrate, fps: 15, screen: false }) {
            Ok(encoder) => encoder,
            Err(words) => {
                log::error!("tinyclient: selftest codec: no encoder at {width}x{height}: {words}");
                return;
            }
        };
        let (mut units, mut bytes, mut idr, mut sps) = (0, 0, 0, 0);
        let mut spent = Duration::ZERO;
        for n in 0..45 {
            let frame = picture(width, height, n);
            let at = Instant::now();
            let coded = encoder.encode(&frame, n == 0 || n == 30);
            spent += at.elapsed();
            match coded {
                Ok(out) if out.is_empty() => {}
                Ok(out) => {
                    let types = nal_types(&out);
                    units += 1;
                    bytes += out.len();
                    idr += types.iter().filter(|&&t| t == 5).count();
                    sps += types.iter().filter(|&&t| t == 7).count();
                    if n < 3 || types.contains(&5) {
                        log::info!("tinyclient: selftest codec: {width}x{height} frame {n}: {} bytes, NAL types {types:?}", out.len());
                    }
                    if width == 640 {
                        high_stream.push(out);
                    }
                }
                Err(words) => log::error!("tinyclient: selftest codec: encode {width}x{height} frame {n}: {words}"),
            }
        }
        log::info!(
            "tinyclient: selftest codec: {width}x{height} at {bitrate} bit/s: 45 frames in, {units} access units out, {bytes} bytes, {idr} IDR, {sps} SPS, {:.1} ms an encode call",
            spent.as_secs_f64() * 1000.0 / 45.0
        );
    }
    let mut decoder = match platform.h264_decoder() {
        Ok(decoder) => decoder,
        Err(words) => {
            log::error!("tinyclient: selftest codec: no decoder: {words}");
            return;
        }
    };
    let mut pictures = 0;
    let mut size = (0, 0);
    for unit in &high_stream {
        match decoder.decode(unit) {
            Ok(Some(frame)) => {
                pictures += 1;
                size = (frame.width, frame.height);
            }
            Ok(None) => {}
            Err(words) => log::error!("tinyclient: selftest codec: decode: {words}"),
        }
    }
    log::info!("tinyclient: selftest codec: decoded {pictures} pictures of {} access units, {}x{}", high_stream.len(), size.0, size.1);
    log::info!("tinyclient: selftest codec: done");
}
