//! H.264 through MediaCodec (`AvcEncoder`, `AvcDecoder` in Java), used synchronously from the
//! call loop: NV12 in, Annex B out; Annex B in, I420 out.

use super::jvm::{self, DECODER, ENCODER};
use jni::objects::{GlobalRef, JByteArray, JObject, JValue};
use mediad::platform::{EncoderConfig, H264Decoder, H264Encoder, PixelFormat, VideoFrame};

/// Bytes of a Java `byte[]`, or `None` for null.
fn bytes(env: &mut jni::JNIEnv, value: JObject) -> jni::errors::Result<Option<Vec<u8>>> {
    if value.is_null() {
        return Ok(None);
    }
    let array = JByteArray::from(value);
    Ok(Some(env.convert_byte_array(&array)?))
}

pub struct Encoder {
    codec: GlobalRef,
    config: EncoderConfig,
}

pub fn encoder(config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String> {
    let codec = jvm::call("start the video encoder", |env| {
        let made = jvm::call_static(
            env,
            ENCODER,
            "create",
            "(IIIIZ)Ldev/tinyclient/app/AvcEncoder;",
            &[
                JValue::Int(config.width as i32),
                JValue::Int(config.height as i32),
                JValue::Int(config.bitrate_bps as i32),
                JValue::Int(config.fps.max(1) as i32),
                JValue::Bool(config.screen.into()),
            ],
        )?
        .l()?;
        if made.is_null() {
            return Ok(None);
        }
        Ok(Some(env.new_global_ref(made)?))
    })?;
    match codec {
        Some(codec) => Ok(Box::new(Encoder { codec, config })),
        None => Err("This phone has no H.264 encoder for video.".to_owned()),
    }
}

impl H264Encoder for Encoder {
    fn encode(&mut self, frame: &VideoFrame, keyframe: bool) -> Result<Vec<u8>, String> {
        if frame.format != PixelFormat::Nv12 || frame.width != self.config.width || frame.height != self.config.height || !frame.is_whole() {
            return Err("The video encoder got a picture it wasn't set up for.".to_owned());
        }
        let codec = self.codec.clone();
        let out = jvm::call("encode video", |env| {
            let input = env.byte_array_from_slice(&frame.data)?;
            let made = env
                .call_method(codec.as_obj(), "encode", "([BJZ)[B", &[JValue::Object(&input), JValue::Long(frame.timestamp_us as i64), JValue::Bool(keyframe.into())])?
                .l()?;
            bytes(env, made)
        })?;
        out.ok_or_else(|| "The video encoder stopped.".to_owned())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let codec = self.codec.clone();
        let _ = jvm::call("stop the video encoder", |env| env.call_method(codec.as_obj(), "release", "()V", &[]).map(|_| ()));
    }
}

pub struct Decoder {
    codec: GlobalRef,
    count: u64,
}

pub fn decoder() -> Result<Box<dyn H264Decoder>, String> {
    let codec = jvm::call("start the video decoder", |env| {
        let made = jvm::call_static(env, DECODER, "create", "()Ldev/tinyclient/app/AvcDecoder;", &[])?.l()?;
        if made.is_null() {
            return Ok(None);
        }
        Ok(Some(env.new_global_ref(made)?))
    })?;
    match codec {
        Some(codec) => Ok(Box::new(Decoder { codec, count: 0 })),
        None => Err("This phone has no H.264 decoder for video.".to_owned()),
    }
}

impl H264Decoder for Decoder {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>, String> {
        self.count += 1;
        let at = self.count * 66_666;
        let codec = self.codec.clone();
        let out = jvm::call("decode video", |env| {
            let input = env.byte_array_from_slice(annex_b)?;
            let made = env.call_method(codec.as_obj(), "decode", "([BJ)[B", &[JValue::Object(&input), JValue::Long(at as i64)])?.l()?;
            bytes(env, made)
        })?;
        let Some(out) = out else {
            return Err("The video decoder stopped.".to_owned());
        };
        Ok(super::wire::decoded_picture(&out).map(|(width, height, data)| VideoFrame { width, height, format: PixelFormat::I420, data: data.to_vec(), timestamp_us: at }))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let codec = self.codec.clone();
        let _ = jvm::call("stop the video decoder", |env| env.call_method(codec.as_obj(), "release", "()V", &[]).map(|_| ()));
    }
}
