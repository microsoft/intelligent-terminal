//! OSC 52 clipboard-set escape sequence.
//!
//! Writes the given text to the terminal's system clipboard via the
//! `ESC ] 52 ; c ; <base64> BEL` escape sequence. No OS-level clipboard API
//! is used, so this works through SSH and other remote sessions as long as
//! the host terminal supports OSC 52 (Windows Terminal does since 1.13).

use std::io::Write;

const B64: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648) — implemented inline so this module doesn't
/// pull in the `base64` crate just for clipboard plumbing.
fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(B64[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64[((n >> 6) & 0x3F) as usize] as char);
        out.push(B64[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(B64[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(B64[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

/// Sends the given text to the terminal's system clipboard.
///
/// Errors are silently swallowed — clipboard copy is best-effort UX, not a
/// critical operation. If stdout is closed or the terminal doesn't honor
/// OSC 52, the user simply doesn't get clipboard contents.
#[allow(dead_code)]
pub fn copy(text: &str) {
    let encoded = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", encoded);
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_known_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
