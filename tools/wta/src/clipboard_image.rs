//! Windows clipboard image reader for the agent-pane **Alt+V** image paste
//! (issue #211).
//!
//! `wta-helper` is a real Win32 process hosted in a conpty pane, so it can read
//! the OS clipboard directly. crossterm only ever delivers *text* paste events,
//! so the image path is handled out-of-band here, on demand, when the user
//! presses Alt+V.
//!
//! The result is an encoded image ready to drop into an ACP
//! `ContentBlock::Image` (`data` = standard base64, `mime_type` = IANA type).
//!
//! Source preference, most faithful first:
//!   1. `CF_HDROP` — an image *file* copied in Explorer. Sent raw (no re-encode)
//!      so quality and format are preserved; mime is derived from the extension.
//!   2. Registered `"PNG"` clipboard format — modern apps (browsers, Snipping
//!      Tool) publish real PNG bytes alongside the DIB. Sent raw.
//!   3. `CF_DIBV5` / `CF_DIB` — the classic bitmap a screenshot lands as. We
//!      wrap it into a BMP file and re-encode to PNG (LLM image inputs reject
//!      raw BMP), via the `image` crate.

use crate::osc52::base64_encode;

/// An image captured from the clipboard, encoded for an ACP image content
/// block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedImage {
    /// Standard (RFC 4648) base64 of the encoded image bytes.
    pub data_base64: String,
    /// IANA mime type, e.g. `image/png`.
    pub mime_type: String,
    /// Best-effort human label for the inline input placeholder
    /// (file name, or `screenshot` / `image`).
    pub label: String,
}

// Classic clipboard format numbers (winuser.h). Hard-coded rather than pulled
// from a windows-sys feature so the dependency surface stays minimal.
#[cfg(windows)]
const CF_DIB: u32 = 8;
#[cfg(windows)]
const CF_DIBV5: u32 = 17;

// BITMAPINFOHEADER biCompression values (wingdi.h).
const BI_BITFIELDS: u32 = 3;

/// Upper bound on a single clipboard payload we will copy into memory. A
/// corrupted or hostile `GlobalSize` could otherwise drive an unbounded
/// allocation (OOM) in the helper just from an Alt+V keypress. 256 MiB is far
/// above any realistic screenshot DIB (a 4K 32-bpp frame is ~33 MiB) yet bounds
/// the worst case. Oversized payloads are rejected (too large to paste).
#[cfg(windows)]
const MAX_CLIPBOARD_BYTES: usize = 256 * 1024 * 1024;

/// Read an image from the Windows clipboard, if one is present.
///
/// Returns `None` when the clipboard holds no image (or only text), the
/// clipboard can't be opened, or decoding fails — callers treat `None` as
/// "nothing to paste".
pub fn read_clipboard_image() -> Option<PastedImage> {
    #[cfg(windows)]
    {
        unsafe { read_clipboard_image_win() }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Wrap a raw `CF_DIB` / `CF_DIBV5` payload in a 14-byte `BITMAPFILEHEADER` so
/// it becomes a self-describing BMP the `image` crate can decode.
///
/// The only non-trivial field is `bfOffBits` (offset to the pixel data): it has
/// to skip the DIB header, any `BI_BITFIELDS` color masks (present inline only
/// for the 40-byte `BITMAPINFOHEADER`; the V4/V5 headers carry masks within the
/// header itself), and any palette.
pub(crate) fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let read_u32 = |off: usize| u32::from_le_bytes(dib[off..off + 4].try_into().unwrap());
    let read_u16 = |off: usize| u16::from_le_bytes(dib[off..off + 2].try_into().unwrap());

    let header_size = read_u32(0);
    let bit_count = read_u16(14);
    let compression = read_u32(16);
    let clr_used = read_u32(32);

    let mut extra = 0usize;
    // Inline color masks live right after a bare BITMAPINFOHEADER (size 40) when
    // BI_BITFIELDS is set; V4 (108) / V5 (124) headers already include them.
    if header_size == 40 && compression == BI_BITFIELDS {
        extra += 12; // 3 × DWORD (R/G/B masks)
    }
    // Palette for indexed-color bitmaps.
    if bit_count <= 8 {
        let colors = if clr_used != 0 {
            clr_used as usize
        } else {
            1usize << bit_count
        };
        extra += colors * 4;
    }

    let off_bits = 14 + header_size as usize + extra;
    let file_size = 14 + dib.len();

    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // bfReserved1/2
    out.extend_from_slice(&(off_bits as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// Decode BMP bytes and re-encode as PNG.
pub(crate) fn bmp_to_png(bmp: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(bmp, image::ImageFormat::Bmp).ok()?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
    Some(cursor.into_inner())
}

/// Map a file extension to an image mime type, or `None` if it isn't a
/// recognized image extension.
pub(crate) fn mime_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Encode an image file at `path` into a [`PastedImage`].
///
/// Already-compressed formats (png/jpeg/gif/webp) are sent raw to preserve
/// fidelity; BMP is re-encoded to PNG because LLM image inputs reject BMP.
pub(crate) fn image_from_path(path: &std::path::Path) -> Option<PastedImage> {
    let ext = path.extension()?.to_str()?;
    let mime = mime_for_extension(ext)?;
    let bytes = std::fs::read(path).ok()?;
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();

    if mime == "image/bmp" {
        let png = bmp_to_png(&bytes)?;
        return Some(PastedImage {
            data_base64: base64_encode(&png),
            mime_type: "image/png".to_string(),
            label,
        });
    }
    Some(PastedImage {
        data_base64: base64_encode(&bytes),
        mime_type: mime.to_string(),
        label,
    })
}

#[cfg(windows)]
unsafe fn read_clipboard_image_win() -> Option<PastedImage> {
    let _guard = ClipboardGuard::open()?;

    // 1. A copied image *file* (CF_HDROP).
    if let Some(path) = crate::win32::clipboard_file_path_from_open_clipboard() {
        if let Some(img) = image_from_path(&path) {
            return Some(img);
        }
    }

    // 2. A registered "PNG" payload (already compressed — send raw).
    let png_fmt = register_format("PNG");
    if png_fmt != 0 {
        if let Some(bytes) = clipboard_bytes(png_fmt) {
            if !bytes.is_empty() {
                return Some(PastedImage {
                    data_base64: base64_encode(&bytes),
                    mime_type: "image/png".to_string(),
                    label: "image".to_string(),
                });
            }
        }
    }

    // 3. A device-independent bitmap (the screenshot path).
    for fmt in [CF_DIBV5, CF_DIB] {
        if let Some(dib) = clipboard_bytes(fmt) {
            if let Some(bmp) = dib_to_bmp(&dib) {
                if let Some(png) = bmp_to_png(&bmp) {
                    return Some(PastedImage {
                        data_base64: base64_encode(&png),
                        mime_type: "image/png".to_string(),
                        label: "screenshot".to_string(),
                    });
                }
            }
        }
    }

    None
}

/// RAII clipboard lock. `OpenClipboard` can transiently fail while another app
/// holds the clipboard, so retry briefly.
#[cfg(windows)]
struct ClipboardGuard;

#[cfg(windows)]
impl ClipboardGuard {
    unsafe fn open() -> Option<Self> {
        use windows_sys::Win32::System::DataExchange::OpenClipboard;
        for _ in 0..10 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                return Some(ClipboardGuard);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}

#[cfg(windows)]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::DataExchange::CloseClipboard;
        unsafe {
            CloseClipboard();
        }
    }
}

/// Copy the bytes behind a clipboard format's `HGLOBAL` into an owned `Vec`.
/// Must be called while the clipboard is open (see [`ClipboardGuard`]).
#[cfg(windows)]
unsafe fn clipboard_bytes(format: u32) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    if IsClipboardFormatAvailable(format) == 0 {
        return None;
    }
    let handle = GetClipboardData(format);
    if handle.is_null() {
        return None;
    }
    let ptr = GlobalLock(handle);
    if ptr.is_null() {
        return None;
    }
    let size = GlobalSize(handle);
    // Guard against an unbounded (or corrupted) size driving an OOM allocation.
    if size == 0 || size > MAX_CLIPBOARD_BYTES {
        GlobalUnlock(handle);
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
    GlobalUnlock(handle);
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}



#[cfg(windows)]
unsafe fn register_format(name: &str) -> u32 {
    use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    RegisterClipboardFormatW(wide.as_ptr())
}

/// Test-only: a process-global lock serializing every test that touches the
/// *live* OS clipboard. The clipboard is global per-desktop state, so without
/// this two parallel tests could interleave their `EmptyClipboard` /
/// `SetClipboardData` / read and see each other's data.
#[cfg(test)]
pub(crate) static CLIPBOARD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only: build the minimal 2×2 24-bpp top-down DIB a screenshot lands on
/// the clipboard as (a bare `BITMAPINFOHEADER`, `BI_RGB`, no palette), suitable
/// for [`set_clipboard_dib`].
#[cfg(test)]
pub(crate) fn sample_screenshot_dib() -> Vec<u8> {
    let width: i32 = 2;
    let height: i32 = -2; // negative => top-down rows
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&width.to_le_bytes()); // biWidth
    dib.extend_from_slice(&height.to_le_bytes()); // biHeight
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    for _ in 0..2 {
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // BGR pixels + row padding
    }
    dib
}

/// Test-only: put a `CF_DIB` payload on the OS clipboard (simulating a
/// screenshot copy). Returns `false` if the clipboard can't be opened / written
/// — e.g. a headless, locked, or sandboxed session — so callers can skip the
/// test instead of failing where no clipboard is available.
#[cfg(all(test, windows))]
pub(crate) unsafe fn set_clipboard_dib(dib: &[u8]) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    if OpenClipboard(std::ptr::null_mut()) == 0 {
        return false;
    }
    // Wipe any preexisting content so the read is deterministic (only our DIB).
    EmptyClipboard();
    let handle = GlobalAlloc(GMEM_MOVEABLE, dib.len());
    if handle.is_null() {
        CloseClipboard();
        return false;
    }
    let ptr = GlobalLock(handle);
    if ptr.is_null() {
        // Ownership was not handed to the clipboard — free our allocation.
        GlobalFree(handle);
        CloseClipboard();
        return false;
    }
    std::ptr::copy_nonoverlapping(dib.as_ptr(), ptr as *mut u8, dib.len());
    GlobalUnlock(handle);
    // On success the system takes ownership of `handle`; on failure it does not,
    // so we must free it ourselves to avoid leaking the allocation.
    let placed = !SetClipboardData(CF_DIB, handle as _).is_null();
    if !placed {
        GlobalFree(handle);
    }
    CloseClipboard();
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 2×2 24-bpp top-down DIB (BITMAPINFOHEADER) and confirm
    /// the BMP wrapper + PNG re-encode round-trips to a decodable 2×2 image.
    #[test]
    fn dib_round_trips_to_png() {
        // BITMAPINFOHEADER: 40 bytes.
        let width: i32 = 2;
        let height: i32 = -2; // negative => top-down rows
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&width.to_le_bytes()); // biWidth
        dib.extend_from_slice(&height.to_le_bytes()); // biHeight
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
        dib.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
                                                    // Pixel rows: 2 px * 3 bytes = 6, padded to a 4-byte boundary => 8 bytes/row.
        for _ in 0..2 {
            dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // BGR pixels + padding
        }

        let bmp = dib_to_bmp(&dib).expect("bmp wrap");
        assert_eq!(&bmp[0..2], b"BM");
        // bfOffBits = 14 + 40 (no palette, no masks).
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);

        let png = bmp_to_png(&bmp).expect("png encode");
        let decoded =
            image::load_from_memory_with_format(&png, image::ImageFormat::Png).expect("decode png");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 2);
    }

    #[test]
    fn dib_off_bits_accounts_for_palette() {
        // 8-bpp indexed bitmap with a full 256-entry palette.
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&1i32.to_le_bytes()); // biWidth
        dib.extend_from_slice(&1i32.to_le_bytes()); // biHeight
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&8u16.to_le_bytes()); // biBitCount = 8
        dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression
        dib.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        dib.extend_from_slice(&0i32.to_le_bytes());
        dib.extend_from_slice(&0i32.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed = 0 => 256 colors
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.resize(dib.len() + 256 * 4, 0); // palette
        dib.resize(dib.len() + 4, 0); // 1 padded pixel row

        let bmp = dib_to_bmp(&dib).expect("bmp wrap");
        // 14 + 40 + 256*4 = 1078.
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 1078);
    }

    #[test]
    fn mime_lookup_is_case_insensitive_and_bounded() {
        assert_eq!(mime_for_extension("PNG"), Some("image/png"));
        assert_eq!(mime_for_extension("jpeg"), Some("image/jpeg"));
        assert_eq!(mime_for_extension("Webp"), Some("image/webp"));
        assert_eq!(mime_for_extension("txt"), None);
    }

    #[test]
    fn short_dib_is_rejected() {
        assert!(dib_to_bmp(&[0u8; 10]).is_none());
    }

    /// End-to-end of the **capture** half of Alt+V: a screenshot-shaped DIB
    /// placed on the real OS clipboard must read back through
    /// [`read_clipboard_image`] as a PNG `PastedImage` whose bytes match what
    /// our pure DIB→BMP→PNG pipeline produces. Skips gracefully where the
    /// session has no clipboard (CI sandbox / headless).
    #[cfg(windows)]
    #[test]
    fn live_clipboard_dib_reads_back_as_screenshot_png() {
        let _guard = CLIPBOARD_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dib = sample_screenshot_dib();
        if !unsafe { set_clipboard_dib(&dib) } {
            eprintln!("skipping live_clipboard_dib_reads_back_as_screenshot_png: clipboard unavailable");
            return;
        }

        let pasted =
            read_clipboard_image().expect("a DIB on the clipboard must read back as an image");
        assert_eq!(pasted.mime_type, "image/png");
        assert_eq!(pasted.label, "screenshot");

        // The captured bytes must be exactly what the pure pipeline yields for
        // the same DIB (proves the clipboard read path, not just that *some*
        // image came back).
        let expected =
            base64_encode(&bmp_to_png(&dib_to_bmp(&dib).unwrap()).unwrap());
        assert_eq!(pasted.data_base64, expected);

        // And the base64 decodes to real PNG bytes (signature check).
        let png = bmp_to_png(&dib_to_bmp(&dib).unwrap()).unwrap();
        assert_eq!(&png[0..8], &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
    }
}
