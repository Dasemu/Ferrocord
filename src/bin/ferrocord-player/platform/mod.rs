//! Platform decoders: what the operating system already provides, used the
//! way Chromium uses it, for the streams the built-in decoders cannot
//! handle (H.265, AV1, H.264 High 10 / 4:2:2 / 4:4:4 / interlaced, AAC in
//! Matroska).
//!
//! - **Linux:** the system FFmpeg libraries (`libavcodec.so.62` down to
//!   `.58`, with the matching `libavutil`), opened with `dlopen` at run time
//!   through a tiny hand-written FFI subset ([`ffmpeg`]). Nothing is linked
//!   at build time: without FFmpeg the helper starts and plays what the
//!   built-in decoders handle, and names the package to install for the rest.
//! - **Windows:** Media Foundation decoder transforms ([`mf`]): the H.264 and
//!   AAC decoders every Windows has, H.265 with "HEVC Video Extensions" and
//!   AV1 with "AV1 Video Extension". `mfplat.dll` is loaded at run time too,
//!   so Windows N editions without the Media Feature Pack still start.
//!
//! The built-in decoders stay the default for what they handle well
//! (OpenH264 for 8-bit 4:2:0 H.264, VP8, VP9); see [`super::codec::new_decoder`].

use super::audio::FrameDecoder;
use super::codec::{Geometry, VideoDecoder};
use super::source::{Codec, VideoInfo};
use super::yuv::{Depth, Layout};

#[cfg(target_os = "linux")]
mod ffmpeg;
#[cfg(target_os = "linux")]
use ffmpeg as imp;
#[cfg(windows)]
mod mf;
#[cfg(windows)]
use mf as imp;

/// No platform decoders elsewhere (the helper is built for Linux and
/// Windows).
#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::*;
    pub fn video(_: &VideoInfo, _: Geometry) -> Result<Box<dyn VideoDecoder>, Missing> {
        Err(Missing { hint: None })
    }
    pub fn aac(_: &[u8]) -> Result<Box<dyn FrameDecoder>, Missing> {
        Err(Missing { hint: None })
    }
    pub fn report() -> Vec<String> {
        vec!["platform none".into()]
    }
    pub fn hint(_: &str, _: Option<&str>) -> Option<String> {
        None
    }
}

/// Set to `0` / `off` to never use platform decoders (the helper then
/// behaves as on a machine without them).
pub const DISABLE_ENV: &str = "FERROCORD_PLATFORM_DECODERS";
/// Set to `platform` to prefer platform decoders for every stream they
/// handle, built-in ones included (troubleshooting and tests).
pub const PREFER_ENV: &str = "FERROCORD_VIDEO_DECODER";

/// Platform decoders may be used.
pub fn enabled() -> bool {
    !matches!(
        std::env::var(DISABLE_ENV).as_deref(),
        Ok("0" | "off" | "false" | "no")
    )
}

/// Platform decoders go first even where a built-in one exists.
pub fn preferred() -> bool {
    enabled() && std::env::var(PREFER_ENV).as_deref() == Ok("platform")
}

/// No platform decoder takes the stream; `hint` says what the user can
/// install to change that (`None`: nothing would).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// One sentence for the viewer.
    pub hint: Option<String>,
}

/// A platform video decoder for `info`.
pub fn video(info: &VideoInfo, geo: Geometry) -> Result<Box<dyn VideoDecoder>, Missing> {
    if !enabled() {
        return Err(Missing {
            hint: install_hint(info.codec.name(), None),
        });
    }
    imp::video(info, geo)
}

/// A platform AAC decoder for raw frames described by `asc` (the MPEG-4
/// AudioSpecificConfig).
pub fn aac(asc: &[u8]) -> Result<Box<dyn FrameDecoder>, Missing> {
    if !enabled() {
        return Err(Missing {
            hint: install_hint("AAC audio", None),
        });
    }
    imp::aac(asc)
}

/// What the platform offers, one line each: `platform <name> ...` then
/// `<codec> <decoder>` or `<codec> -` for h264, hevc, av1, vp9, aac.
/// Printed by `ferrocord-player --backends`.
pub fn report() -> Vec<String> {
    if !enabled() {
        return vec![format!("platform none (turned off by {DISABLE_ENV})")];
    }
    imp::report()
}

/// The short codec keys [`report`] uses.
pub fn key(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
        Codec::Vp8 => "vp8",
        Codec::Vp9 => "vp9",
    }
}

/// The sentence telling the user how to get a decoder for `what` on this
/// system. `lib` describes what was found: `None` when no platform decoder
/// library is there at all, `Some(name)` when it is there without this codec.
pub fn install_hint(what: &str, lib: Option<&str>) -> Option<String> {
    imp::hint(what, lib)
}

/// `ID` and `ID_LIKE` of `/etc/os-release`, lower case.
#[cfg(target_os = "linux")]
fn os_release_ids() -> Vec<String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    os_ids(&text)
}

/// `ID` and `ID_LIKE` values of an os-release file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn os_ids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")))
        .flat_map(|v| {
            v.trim_matches(|c| c == '"' || c == '\'')
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Linux: name the distribution's FFmpeg package.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn linux_hint(what: &str, lib: Option<&str>, ids: &[String]) -> String {
    let is = |d: &str| ids.iter().any(|i| i == d);
    match lib {
        // FFmpeg is there, without this decoder: Fedora's default
        // `libavcodec-free` leaves out H.265 (and H.264 / AAC in older
        // releases); the full build is RPM Fusion's `ffmpeg-libs`.
        Some(_) if is("fedora") || is("rhel") => format!(
            "The installed FFmpeg (Fedora's libavcodec-free) has no {what} decoder: install \
             ffmpeg-libs from RPM Fusion to play it here."
        ),
        Some(_) if is("opensuse") || is("suse") => format!(
            "The installed FFmpeg has no {what} decoder: install libavcodec from Packman to \
             play it here."
        ),
        Some(_) if what == "AV1" => {
            "The installed FFmpeg has no AV1 decoder: install libdav1d to play it here.".into()
        }
        Some(_) => format!(
            "The installed FFmpeg has no {what} decoder: install a complete FFmpeg build to \
             play it here."
        ),
        None if is("fedora") || is("rhel") => {
            "Install the FFmpeg libraries (ffmpeg-libs from RPM Fusion) to play it here.".into()
        }
        None if is("arch") || is("manjaro") => {
            "Install the FFmpeg libraries (package ffmpeg) to play it here.".into()
        }
        None if is("opensuse") || is("suse") => {
            "Install the FFmpeg libraries (libavcodec61 from Packman) to play it here.".into()
        }
        None if is("debian") || is("ubuntu") => {
            "Install the FFmpeg libraries (package libavcodec61, or libavcodec60 on Ubuntu \
             24.04) to play it here."
                .into()
        }
        None => "Install the FFmpeg libraries (libavcodec, e.g. libavcodec61 or ffmpeg-libs) to \
                 play it here."
            .into(),
    }
}

/// Windows: the Microsoft Store extension that adds the decoder.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_hint(what: &str, media_foundation: bool) -> Option<String> {
    if !media_foundation {
        return Some(
            "Install the Media Feature Pack (Settings, Apps, Optional features) to play it here."
                .into(),
        );
    }
    if what.contains("HEVC") {
        Some(
            "Install \u{201c}HEVC Video Extensions from Device Manufacturer\u{201d} from the \
             Microsoft Store to play it here."
                .into(),
        )
    } else if what.contains("AV1") {
        Some(
            "Install \u{201c}AV1 Video Extension\u{201d} from the Microsoft Store to play it here."
                .into(),
        )
    } else if what.contains("VP9") {
        Some(
            "Install \u{201c}VP9 Video Extensions\u{201d} from the Microsoft Store to play it here."
                .into(),
        )
    } else {
        None
    }
}

/// The layout of an FFmpeg pixel format, by name (enum values moved
/// between FFmpeg versions, names did not). `None` for formats the helper
/// does not convert (RGB, palettes, 4:1:1, ...).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn ffmpeg_layout(name: &str) -> Option<Layout> {
    match name {
        "yuv420p" | "yuvj420p" => return Some(Layout::I420),
        "nv12" => return Some(Layout::NV12),
        "p010le" | "p012le" | "p016le" => return Some(Layout::P010),
        _ => {}
    }
    let rest = name
        .strip_prefix("yuvj")
        .or_else(|| name.strip_prefix("yuv"))?;
    let (sub_x, sub_y) = match rest.get(..3)? {
        "420" => (1, 1),
        "422" => (1, 0),
        "444" => (0, 0),
        "440" => (0, 1),
        _ => return None,
    };
    let depth = match rest.get(3..)? {
        "p" => Depth::Eight,
        bits => {
            let bits: u32 = bits.strip_prefix('p')?.strip_suffix("le")?.parse().ok()?;
            if !(9..=16).contains(&bits) {
                return None;
            }
            Depth::Wide { shift: bits - 8 }
        }
    };
    Some(Layout {
        sub_x,
        sub_y,
        interleaved: false,
        depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_pixel_formats_map_to_layouts() {
        assert_eq!(ffmpeg_layout("yuv420p"), Some(Layout::I420));
        assert_eq!(ffmpeg_layout("yuvj420p"), Some(Layout::I420));
        assert_eq!(ffmpeg_layout("nv12"), Some(Layout::NV12));
        assert_eq!(ffmpeg_layout("p010le"), Some(Layout::P010));
        assert_eq!(
            ffmpeg_layout("yuv420p10le"),
            Some(Layout {
                depth: Depth::Wide { shift: 2 },
                ..Layout::I420
            })
        );
        let l = ffmpeg_layout("yuv422p10le").unwrap();
        assert_eq!(
            (l.sub_x, l.sub_y, l.depth),
            (1, 0, Depth::Wide { shift: 2 })
        );
        let l = ffmpeg_layout("yuv444p").unwrap();
        assert_eq!((l.sub_x, l.sub_y, l.depth), (0, 0, Depth::Eight));
        let l = ffmpeg_layout("yuv444p12le").unwrap();
        assert_eq!(l.depth, Depth::Wide { shift: 4 });
        for bad in [
            "rgb24",
            "yuv411p",
            "yuv420p10be",
            "yuv420p8le",
            "gray",
            "",
            "yuv",
            "yuv420",
        ] {
            assert_eq!(ffmpeg_layout(bad), None, "{bad}");
        }
    }

    #[test]
    fn os_release_ids_include_the_family() {
        let ids = os_ids("NAME=\"Fedora Linux\"\nID=fedora\nVERSION_ID=41\n");
        assert_eq!(ids, ["fedora"]);
        let ids = os_ids("ID=kubuntu\nID_LIKE=\"ubuntu debian\"\n");
        assert_eq!(ids, ["kubuntu", "ubuntu", "debian"]);
        assert!(os_ids("").is_empty());
    }

    #[test]
    fn linux_hints_name_the_package() {
        let ids = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let h = linux_hint("H.265 (HEVC)", Some("libavcodec.so.61"), &ids(&["fedora"]));
        assert!(
            h.contains("libavcodec-free") && h.contains("ffmpeg-libs"),
            "{h}"
        );
        let h = linux_hint("H.265 (HEVC)", None, &ids(&["ubuntu", "debian"]));
        assert!(h.contains("libavcodec61"), "{h}");
        let h = linux_hint("AV1", Some("x"), &ids(&["debian"]));
        assert!(h.contains("libdav1d"), "{h}");
        let h = linux_hint("AV1", None, &ids(&["arch"]));
        assert!(h.contains("ffmpeg"), "{h}");
        let h = linux_hint("AV1", None, &[]);
        assert!(h.contains("libavcodec"), "{h}");
        // Short enough for the viewer (it keeps 200 characters).
        for d in ["fedora", "ubuntu", "arch", "opensuse", "nixos"] {
            for lib in [None, Some("x")] {
                assert!(linux_hint("H.265 (HEVC)", lib, &ids(&[d])).len() < 200);
            }
        }
    }

    #[test]
    fn windows_hints_name_the_store_extension() {
        let h = windows_hint("H.265 (HEVC)", true).unwrap();
        assert!(
            h.contains("HEVC Video Extensions from Device Manufacturer"),
            "{h}"
        );
        let h = windows_hint("AV1", true).unwrap();
        assert!(h.contains("AV1 Video Extension"), "{h}");
        assert_eq!(windows_hint("10-bit H.264 (High 10 profile)", true), None);
        assert!(
            windows_hint("AV1", false)
                .unwrap()
                .contains("Media Feature Pack")
        );
    }
}
