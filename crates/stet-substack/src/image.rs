//! Local pictures as the `data:` URIs the upload endpoint takes.

use crate::error::{Error, Result};

/// The upload limit is not known. This is a client-side guard, not Substack's.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// The type, read from the file's first bytes rather than its name.
pub fn sniff(bytes: &[u8]) -> Result<&'static str> {
    let at = |from: usize, to: usize| bytes.get(from..to).unwrap_or(&[]);
    if at(0, 8) == b"\x89PNG\r\n\x1a\n" {
        Ok("image/png")
    } else if at(0, 3) == [0xff, 0xd8, 0xff] {
        Ok("image/jpeg")
    } else if at(0, 6) == b"GIF87a" || at(0, 6) == b"GIF89a" {
        Ok("image/gif")
    } else if at(0, 4) == b"RIFF" && at(8, 12) == b"WEBP" {
        Ok("image/webp")
    } else if at(4, 8) == b"ftyp" && matches!(at(8, 12), b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc") {
        Err(Error::Image(
            "HEIC is not accepted by Substack; convert to JPEG or PNG first".into(),
        ))
    } else {
        Err(Error::Image("not a PNG, JPEG, GIF or WebP image".into()))
    }
}

pub fn data_uri(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Image(format!(
            "image is {} bytes, over the {MAX_BYTES} byte limit",
            bytes.len()
        )));
    }
    let mime = sniff(bytes)?;
    Ok(format!("data:{mime};base64,{}", base64(bytes)))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }

    #[test]
    fn sniffs_by_content() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....").unwrap(), "image/png");
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]).unwrap(), "image/jpeg");
        assert_eq!(sniff(b"GIF89a..").unwrap(), "image/gif");
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").unwrap(), "image/webp");
        assert!(sniff(b"").is_err());
        assert!(sniff(b"<svg>").is_err());
        assert!(sniff(b"\0\0\0\x18ftypheic").unwrap_err().to_string().contains("HEIC"));
    }

    #[test]
    fn oversize_is_refused() {
        let mut big = b"GIF89a".to_vec();
        big.resize(MAX_BYTES + 1, 0);
        assert!(data_uri(&big).is_err());
    }
}
