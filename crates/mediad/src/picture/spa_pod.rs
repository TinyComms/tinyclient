//! The slice of PipeWire's SPA pods a screen or camera stream has to offer and read back.
//!
//! Built by hand to the same rules as `spa_pod_builder`: a pod's `size` is its body, the next
//! pod starts eight-byte aligned, and a choice's first value is written as a whole pod (that
//! pod is the child header plus the default) while the rest are bare bodies. Compiled on every
//! desktop, so the bytes are tested here and the Linux capture uses the same functions.

use super::pipewire_abi::{
    SPA_CHOICE_ENUM, SPA_CHOICE_RANGE, SPA_FORMAT_VIDEO_FORMAT, SPA_FORMAT_VIDEO_FRAMERATE,
    SPA_FORMAT_VIDEO_SIZE, SPA_MEDIA_SUBTYPE_RAW, SPA_MEDIA_TYPE_VIDEO, SPA_PARAM_ENUM_FORMAT,
    SPA_TYPE_CHOICE, SPA_TYPE_FRACTION, SPA_TYPE_ID, SPA_TYPE_OBJECT, SPA_TYPE_OBJECT_FORMAT,
    SPA_TYPE_RECTANGLE, SPA_VIDEO_FORMAT_BGRA, SPA_VIDEO_FORMAT_BGRX, SPA_VIDEO_FORMAT_NV12,
    SPA_VIDEO_FORMAT_RGBA, SPA_VIDEO_FORMAT_RGBX,
};

const FLAG_BODY: u32 = 1;
const FLAG_FIRST: u32 = 2;

struct Frame {
    /// Where this pod's header `size` sits.
    offset: usize,
    /// Body bytes so far, including what was written before the frame was pushed.
    size: u32,
    parent_flags: u32,
}

struct Builder {
    data: Vec<u8>,
    frames: Vec<Frame>,
    flags: u32,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            data: Vec::new(),
            frames: Vec::new(),
            flags: 0,
        }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        for frame in &mut self.frames {
            frame.size = frame.size.saturating_add(bytes.len() as u32);
        }
    }

    fn u32(&mut self, value: u32) {
        self.raw(&value.to_ne_bytes());
    }

    /// Header only. The body size starts at `size` and is rewritten on [`Builder::pop`].
    fn header(&mut self, size: u32, kind: u32) {
        self.u32(size);
        self.u32(kind);
    }

    fn push_object(&mut self, object_type: u32, id: u32) {
        let offset = self.data.len();
        self.header(8, SPA_TYPE_OBJECT);
        self.u32(object_type);
        self.u32(id);
        let parent_flags = self.flags;
        self.frames.push(Frame {
            offset,
            size: 8,
            parent_flags,
        });
    }

    fn push_choice(&mut self, kind: u32) {
        let offset = self.data.len();
        self.header(8, SPA_TYPE_CHOICE);
        self.u32(kind);
        self.u32(0);
        let parent_flags = self.flags;
        self.frames.push(Frame {
            offset,
            size: 8,
            parent_flags,
        });
        self.flags = FLAG_FIRST | FLAG_BODY;
    }

    fn pop(&mut self) {
        let Some(frame) = self.frames.pop() else {
            return;
        };
        let bytes = frame.size.to_ne_bytes();
        self.data[frame.offset..frame.offset + 4].copy_from_slice(&bytes);
        self.flags = frame.parent_flags;
        let extra = (8 - self.data.len() % 8) % 8;
        if extra > 0 {
            self.raw(&[0u8; 8][..extra]);
        }
    }

    fn id(&mut self, value: u32) {
        if self.flags == FLAG_BODY {
            self.u32(value);
            return;
        }
        self.flags &= !FLAG_FIRST;
        self.header(4, SPA_TYPE_ID);
        self.u32(value);
        if self.flags != FLAG_BODY {
            self.u32(0);
        }
    }

    fn rectangle(&mut self, width: u32, height: u32) {
        if self.flags == FLAG_BODY {
            self.u32(width);
            self.u32(height);
            return;
        }
        self.flags &= !FLAG_FIRST;
        self.header(8, SPA_TYPE_RECTANGLE);
        self.u32(width);
        self.u32(height);
    }

    fn fraction(&mut self, num: u32, denom: u32) {
        if self.flags == FLAG_BODY {
            self.u32(num);
            self.u32(denom);
            return;
        }
        self.flags &= !FLAG_FIRST;
        self.header(8, SPA_TYPE_FRACTION);
        self.u32(num);
        self.u32(denom);
    }

    fn prop_id(&mut self, key: u32, value: u32) {
        self.u32(key);
        self.u32(0);
        self.id(value);
    }

    fn prop_choice_id(&mut self, key: u32, values: &[u32]) {
        self.u32(key);
        self.u32(0);
        self.push_choice(SPA_CHOICE_ENUM);
        for value in values {
            self.id(*value);
        }
        self.pop();
    }

    fn prop_range_rect(&mut self, key: u32, default: (u32, u32), min: (u32, u32), max: (u32, u32)) {
        self.u32(key);
        self.u32(0);
        self.push_choice(SPA_CHOICE_RANGE);
        for (width, height) in [default, min, max] {
            self.rectangle(width, height);
        }
        self.pop();
    }

    fn prop_range_frac(&mut self, key: u32, default: (u32, u32), min: (u32, u32), max: (u32, u32)) {
        self.u32(key);
        self.u32(0);
        self.push_choice(SPA_CHOICE_RANGE);
        for (num, denom) in [default, min, max] {
            self.fraction(num, denom);
        }
        self.pop();
    }
}

/// What a fixated `Format` param says the stream actually is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fixated {
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// The `EnumFormat` a capture offers: raw video, a handful of byte layouts PipeWire maps into
/// ordinary memory, any size, and a frame rate the source may pick inside. No modifier, so a
/// DMA-BUF-only offer is not what we ask for.
pub(super) fn offer_pod() -> Vec<u8> {
    let mut builder = Builder::new();
    builder.push_object(SPA_TYPE_OBJECT_FORMAT, SPA_PARAM_ENUM_FORMAT);
    builder.prop_id(1, SPA_MEDIA_TYPE_VIDEO);
    builder.prop_id(2, SPA_MEDIA_SUBTYPE_RAW);
    builder.prop_choice_id(
        SPA_FORMAT_VIDEO_FORMAT,
        &[
            SPA_VIDEO_FORMAT_BGRX,
            SPA_VIDEO_FORMAT_BGRX,
            SPA_VIDEO_FORMAT_BGRA,
            SPA_VIDEO_FORMAT_RGBX,
            SPA_VIDEO_FORMAT_RGBA,
            SPA_VIDEO_FORMAT_NV12,
        ],
    );
    builder.prop_range_rect(SPA_FORMAT_VIDEO_SIZE, (320, 240), (1, 1), (8192, 8192));
    builder.prop_range_frac(SPA_FORMAT_VIDEO_FRAMERATE, (30, 1), (0, 1), (60, 1));
    builder.pop();
    builder.data
}

/// A fixated format object, the shape `param_changed` delivers once the stream has chosen.
pub(super) fn fixated_pod(format: u32, width: u32, height: u32) -> Vec<u8> {
    let mut builder = Builder::new();
    builder.push_object(
        SPA_TYPE_OBJECT_FORMAT,
        super::pipewire_abi::SPA_PARAM_FORMAT,
    );
    builder.prop_id(SPA_FORMAT_VIDEO_FORMAT, format);
    builder.u32(SPA_FORMAT_VIDEO_SIZE);
    builder.u32(0);
    builder.rectangle(width, height);
    builder.pop();
    builder.data
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_ne_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// The default of a choice, or the value itself when the param is already fixated.
fn scalar(bytes: &[u8], kind: u32, size: u32, at: usize) -> Option<(u32, Option<(u32, u32)>)> {
    if kind == SPA_TYPE_CHOICE {
        if size < 16 {
            return None;
        }
        let child_size = read_u32(bytes, at + 8)?;
        let child_type = read_u32(bytes, at + 12)?;
        return scalar(bytes, child_type, child_size, at + 16);
    }
    if kind == SPA_TYPE_ID && size >= 4 {
        return Some((read_u32(bytes, at)?, None));
    }
    if kind == SPA_TYPE_RECTANGLE && size >= 8 {
        return Some((0, Some((read_u32(bytes, at)?, read_u32(bytes, at + 4)?))));
    }
    None
}

/// Width, height and video format from a `Format` or `EnumFormat` object. A choice contributes
/// its default, which is the first value.
pub(super) fn parse_format(bytes: &[u8]) -> Option<Fixated> {
    if bytes.len() < 16 {
        return None;
    }
    let size = read_u32(bytes, 0)?;
    if read_u32(bytes, 4)? != SPA_TYPE_OBJECT || read_u32(bytes, 8)? != SPA_TYPE_OBJECT_FORMAT {
        return None;
    }
    let end = 8usize.checked_add(size as usize)?;
    if end > bytes.len() {
        return None;
    }
    let mut at = 16usize;
    let mut format = None;
    let mut width = None;
    let mut height = None;
    while at + 16 <= end {
        if at % 8 != 0 {
            at = (at + 7) & !7;
            if at + 16 > end {
                break;
            }
        }
        let key = read_u32(bytes, at)?;
        let value_size = read_u32(bytes, at + 8)?;
        let value_type = read_u32(bytes, at + 12)?;
        let body = at + 16;
        let value_end = body.checked_add(value_size as usize)?;
        if value_end > end {
            return None;
        }
        if let Some((id, rect)) = scalar(bytes, value_type, value_size, body) {
            if key == SPA_FORMAT_VIDEO_FORMAT {
                format = Some(id);
            } else if key == SPA_FORMAT_VIDEO_SIZE {
                if let Some((w, h)) = rect {
                    width = Some(w);
                    height = Some(h);
                }
            }
        }
        let next = (value_end + 7) & !7;
        if next <= at {
            break;
        }
        at = next;
    }
    Some(Fixated {
        format: format?,
        width: width?,
        height: height?,
    })
}

/// Every id in the video-format choice, in offer order. The default is first, and repeated.
pub(super) fn offered_video_formats(bytes: &[u8]) -> Vec<u32> {
    let Some(size) = read_u32(bytes, 0) else {
        return Vec::new();
    };
    let end = 8usize.saturating_add(size as usize).min(bytes.len());
    let mut at = 16usize;
    let mut found = Vec::new();
    while at + 16 <= end {
        if at % 8 != 0 {
            at = (at + 7) & !7;
            continue;
        }
        let key = read_u32(bytes, at).unwrap_or(0);
        let value_size = read_u32(bytes, at + 8).unwrap_or(0);
        let value_type = read_u32(bytes, at + 12).unwrap_or(0);
        let body = at + 16;
        let value_end = body.saturating_add(value_size as usize);
        if value_end > end {
            break;
        }
        if key == SPA_FORMAT_VIDEO_FORMAT && value_type == SPA_TYPE_CHOICE && value_size >= 16 {
            let child_size = read_u32(bytes, body + 8).unwrap_or(0);
            let count = if child_size == 0 {
                0
            } else {
                value_size.saturating_sub(16) / child_size
            };
            let mut cursor = body + 16;
            for _ in 0..count {
                if let Some(id) = read_u32(bytes, cursor) {
                    found.push(id);
                }
                cursor = cursor.saturating_add(child_size as usize);
            }
        }
        let next = (value_end + 7) & !7;
        if next <= at {
            break;
        }
        at = next;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_names_bgrx_and_nv12_and_parses_its_default() {
        let offer = offer_pod();
        assert_eq!(offer.len() % 8, 0);
        let parsed = parse_format(&offer).expect("offer");
        assert_eq!(parsed.format, SPA_VIDEO_FORMAT_BGRX);
        assert_eq!((parsed.width, parsed.height), (320, 240));
        let formats = offered_video_formats(&offer);
        assert!(formats.contains(&SPA_VIDEO_FORMAT_BGRX));
        assert!(formats.contains(&SPA_VIDEO_FORMAT_NV12));
        assert!(formats.contains(&SPA_VIDEO_FORMAT_BGRA));
    }

    #[test]
    fn a_fixated_format_round_trips() {
        let pod = fixated_pod(SPA_VIDEO_FORMAT_BGRA, 1920, 1080);
        let parsed = parse_format(&pod).expect("format");
        assert_eq!(
            parsed,
            Fixated {
                format: SPA_VIDEO_FORMAT_BGRA,
                width: 1920,
                height: 1080
            }
        );
    }
}
