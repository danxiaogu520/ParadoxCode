//! Minimal PNG encoder and base64 helpers for emitting texture data URLs.
//!
//! Kept dependency-free: the encoder uses zlib "stored" blocks (no deflate
//! compression), which is simple, deterministic, and perfectly adequate for
//! small UI sprites like EU4 mission icons.

use super::dds::DecodedImage;

/// One dictionary-free zlib stream stored as a single uncompressed block.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 32);
    out.push(0x78); // CMF: deflate, 32K window.
    out.push(0x01); // FLG: no dictionary, lowest compression.
    out.push(0x01); // BFINAL=1, BTYPE=00 (stored).
    let len = data.len() as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    const POLY: u32 = 0xedb8_8320;
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 12);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let mut crc_bytes = Vec::with_capacity(4 + payload.len());
    crc_bytes.extend_from_slice(kind);
    crc_bytes.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&crc_bytes).to_be_bytes());
    out
}

/// Encodes an RGBA8 image as a PNG (8-bit, truecolor, filter 0 rows).
pub fn encode_png(image: &DecodedImage) -> Vec<u8> {
    let width = image.width as usize;
    let height = image.height as usize;
    let mut raw = Vec::with_capacity((width * 4 + 1) * height);
    for row in image.pixels.chunks_exact(width * 4) {
        raw.push(0); // filter type: none
        raw.extend_from_slice(row);
    }
    let mut out = Vec::with_capacity(raw.len() + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&image.width.to_be_bytes());
    ihdr.extend_from_slice(&image.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, no interlace
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &zlib_stored(&raw)));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

/// Encodes bytes as a base64 string (RFC 4648, no padding accepted).
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(TABLE[(triple >> 18) as usize & 63] as char);
        out.push(TABLE[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Encodes a decoded sprite as a `data:image/png;base64,...` URL.
pub fn png_data_url(image: &DecodedImage) -> String {
    let mut url = String::from("data:image/png;base64,");
    url.push_str(&base64_encode(&encode_png(image)));
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_covers_padding_and_unpadded_lengths() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn encoded_png_has_valid_signature_and_chunks() {
        let image = DecodedImage {
            width: 2,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0],
        };
        let png = encode_png(&image);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[37..41], b"IDAT");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        let url = png_data_url(&image);
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(!url[..22].is_empty());
    }

    #[test]
    fn zlib_stored_round_trips_deterministically() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let stored = zlib_stored(&data);
        // Header (2) + block header (5) + payload + adler (4).
        assert_eq!(stored.len(), data.len() + 11);
        assert_eq!(&stored[..2], &[0x78, 0x01]);
    }
}
