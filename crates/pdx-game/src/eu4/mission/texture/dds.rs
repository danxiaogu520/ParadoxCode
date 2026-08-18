//! Minimal DDS texture decoder for EU4 interface assets.
//!
//! EU4 ships its interface textures as DDS files. Most mission icons, the
//! frame, and several arrow tiles are uncompressed BGRA bitmaps, while a few
//! arrow tiles (vertical runs, horizontal skip) use DXT1/3/5 compression.
//! This decoder covers both and always emits RGBA8, level 0 only.

/// One decoded image: RGBA8 pixels, row-major, no stride padding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Failure to decode a DDS file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DdsError(pub &'static str);

impl std::fmt::Display for DdsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid DDS texture: {}", self.0)
    }
}

impl std::error::Error for DdsError {}

const DDPF_ALPHAPIXELS: u32 = 0x0000_0001;
const DDPF_FOURCC: u32 = 0x0000_0004;
const DDPF_PITCH: u32 = 0x0000_0008;

/// Maximum texture dimension this decoder accepts (guards hostile files).
pub const MAX_TEXTURE_DIMENSION: u32 = 4096;

/// Decodes one DDS file to RGBA8 (mip level 0 only).
pub fn decode_dds(bytes: &[u8]) -> Result<DecodedImage, DdsError> {
    if bytes.len() < 128 || &bytes[..4] != b"DDS " {
        return Err(DdsError("missing DDS header"));
    }
    let read = |offset: usize| -> u32 {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("bounds checked"),
        )
    };
    let height = read(12);
    let width = read(16);
    let pitch = read(20);
    if width == 0 || height == 0 || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION
    {
        return Err(DdsError("invalid texture dimensions"));
    }
    let format_flags = read(80);
    if format_flags & DDPF_FOURCC != 0 {
        let fourcc = &bytes[84..88];
        let block_bytes: u32 = match fourcc {
            b"DXT1" => 8,
            b"DXT3" | b"DXT5" => 16,
            _ => return Err(DdsError("unsupported fourcc (only DXT1/3/5)")),
        };
        let data = bytes.get(128..).ok_or(DdsError("missing pixel data"))?;
        let blocks_w = width.div_ceil(4);
        let blocks_h = height.div_ceil(4);
        let level_bytes = blocks_w * blocks_h * block_bytes;
        let blocks = data
            .get(..level_bytes as usize)
            .ok_or(DdsError("truncated pixel data"))?;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        decode_compressed(fourcc, width, height, blocks, &mut pixels)?;
        Ok(DecodedImage {
            width,
            height,
            pixels,
        })
    } else {
        decode_uncompressed(bytes, width, height, pitch, format_flags)
    }
}

fn decode_uncompressed(
    bytes: &[u8],
    width: u32,
    height: u32,
    pitch: u32,
    format_flags: u32,
) -> Result<DecodedImage, DdsError> {
    let bit_count = u32::from_le_bytes(bytes[88..92].try_into().expect("bounds checked"));
    let r_mask = u32::from_le_bytes(bytes[92..96].try_into().expect("bounds checked"));
    let g_mask = u32::from_le_bytes(bytes[96..100].try_into().expect("bounds checked"));
    let b_mask = u32::from_le_bytes(bytes[100..104].try_into().expect("bounds checked"));
    let a_mask = u32::from_le_bytes(bytes[104..108].try_into().expect("bounds checked"));

    let bytes_per_pixel = match bit_count {
        32 => 4,
        24 => 3,
        _ => return Err(DdsError("unsupported bit depth (only 24/32-bit)")),
    };
    let stride = if format_flags & DDPF_PITCH != 0 {
        pitch as usize
    } else {
        width as usize * bytes_per_pixel
    };
    if stride < width as usize * bytes_per_pixel {
        return Err(DdsError("invalid stride"));
    }
    let data = bytes.get(128..).ok_or(DdsError("missing pixel data"))?;
    let data = data
        .get(..stride.saturating_mul(height as usize))
        .ok_or(DdsError("truncated pixel data"))?;

    let channel = |mask: u32| -> Option<usize> {
        if mask == 0 || mask.count_ones() != 8 {
            return None;
        }
        Some((mask.trailing_zeros() / 8) as usize)
    };
    let r = channel(r_mask);
    let g = channel(g_mask);
    let b = channel(b_mask);
    let a = channel(a_mask).or_else(|| {
        // Bitmaps with an alpha plane keep the fourth byte 255.
        (format_flags & DDPF_ALPHAPIXELS != 0 && bit_count == 32).then_some(3)
    });
    if r.is_none() || g.is_none() || b.is_none() {
        return Err(DdsError("missing or non-8-bit color masks"));
    }

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = &data[y * stride..y * stride + width as usize * bytes_per_pixel];
        for pixel in row.chunks_exact(bytes_per_pixel) {
            pixels.push(pixel[r.unwrap()]);
            pixels.push(pixel[g.unwrap()]);
            pixels.push(pixel[b.unwrap()]);
            pixels.push(a.map_or(255, |offset| pixel[offset]));
        }
    }
    Ok(DecodedImage {
        width,
        height,
        pixels,
    })
}

/// Decodes a compressed block array into `pixels` (RGBA8).
fn decode_compressed(
    fourcc: &[u8],
    width: u32,
    height: u32,
    blocks: &[u8],
    pixels: &mut [u8],
) -> Result<(), DdsError> {
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);
    let block_bytes = blocks.len() / (blocks_w as usize * blocks_h as usize);
    let mut block_index = 0usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block = &blocks[block_index..block_index + block_bytes];
            block_index += block_bytes;
            let rgba = match fourcc {
                b"DXT1" => decode_dxt1_block(block)?,
                b"DXT3" => decode_dxt3_block(block)?,
                b"DXT5" => decode_dxt5_block(block)?,
                _ => return Err(DdsError("unsupported fourcc (only DXT1/3/5)")),
            };
            for py in 0..4 {
                for px in 0..4 {
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= width || y >= height {
                        continue;
                    }
                    let target = (y * width + x) as usize * 4;
                    let source = (py * 4 + px) as usize * 4;
                    pixels[target..target + 4].copy_from_slice(&rgba[source..source + 4]);
                }
            }
        }
    }
    Ok(())
}

fn rgb565(value: u16) -> [u8; 3] {
    let r = ((value >> 11) & 0x1f) as u8;
    let g = ((value >> 5) & 0x3f) as u8;
    let b = (value & 0x1f) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

fn mix(a: u8, b: u8, a_weight: u32, denom: u32) -> u8 {
    ((a as u32 * a_weight + b as u32 * (denom - a_weight)) / denom) as u8
}

/// DXT1 color palette: 4 colors, or 3 colors plus transparent black.
fn dxt1_palette(c0: u16, c1: u16) -> [[u8; 4]; 4] {
    let c0 = rgb565(c0);
    let c1 = rgb565(c1);
    if c0 > c1 {
        [
            [c0[0], c0[1], c0[2], 255],
            [c1[0], c1[1], c1[2], 255],
            [
                mix(c0[0], c1[0], 2, 3),
                mix(c0[1], c1[1], 2, 3),
                mix(c0[2], c1[2], 2, 3),
                255,
            ],
            [
                mix(c0[0], c1[0], 1, 3),
                mix(c0[1], c1[1], 1, 3),
                mix(c0[2], c1[2], 1, 3),
                255,
            ],
        ]
    } else {
        [
            [c0[0], c0[1], c0[2], 255],
            [c1[0], c1[1], c1[2], 255],
            [
                mix(c0[0], c1[0], 1, 2),
                mix(c0[1], c1[1], 1, 2),
                mix(c0[2], c1[2], 1, 2),
                255,
            ],
            [0, 0, 0, 0],
        ]
    }
}

fn decode_dxt1_block(block: &[u8]) -> Result<[u8; 64], DdsError> {
    if block.len() < 8 {
        return Err(DdsError("truncated DXT1 block"));
    }
    let palette = dxt1_palette(
        u16::from_le_bytes([block[0], block[1]]),
        u16::from_le_bytes([block[2], block[3]]),
    );
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let mut out = [0u8; 64];
    for i in 0..16 {
        let code = ((indices >> (i * 2)) & 3) as usize;
        out[i * 4..i * 4 + 4].copy_from_slice(&palette[code]);
    }
    Ok(out)
}

fn decode_dxt3_block(block: &[u8]) -> Result<[u8; 64], DdsError> {
    if block.len() < 16 {
        return Err(DdsError("truncated DXT3 block"));
    }
    let mut out = decode_dxt1_block(&block[8..])?;
    // Explicit 4-bit alpha: two pixels per byte, low nibble first.
    for i in 0..16 {
        let nibble = if i % 2 == 0 {
            block[i / 2] & 0x0f
        } else {
            block[i / 2] >> 4
        };
        out[i * 4 + 3] = nibble * 17;
    }
    Ok(out)
}

fn decode_dxt5_block(block: &[u8]) -> Result<[u8; 64], DdsError> {
    if block.len() < 16 {
        return Err(DdsError("truncated DXT5 block"));
    }
    let a0 = block[0];
    let a1 = block[1];
    let alphas = if a0 > a1 {
        [
            a0,
            a1,
            mix(a0, a1, 6, 7),
            mix(a0, a1, 5, 7),
            mix(a0, a1, 4, 7),
            mix(a0, a1, 3, 7),
            mix(a0, a1, 2, 7),
            mix(a0, a1, 1, 7),
        ]
    } else {
        [
            a0,
            a1,
            mix(a0, a1, 4, 5),
            mix(a0, a1, 3, 5),
            mix(a0, a1, 2, 5),
            mix(a0, a1, 1, 5),
            0,
            255,
        ]
    };
    let mut out = decode_dxt1_block(&block[8..])?;
    for i in 0..16 {
        let bit = i * 3;
        let index = ((u16::from(block[2 + bit / 8]) | (u16::from(block[3 + bit / 8]) << 8))
            >> (bit % 8))
            & 0x07;
        out[i * 4 + 3] = alphas[index as usize];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u32, height: u32, format_flags: u32, fourcc: [u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0u8; 128];
        bytes[..4].copy_from_slice(b"DDS ");
        bytes[4..8].copy_from_slice(&124u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&0x1007u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&height.to_le_bytes());
        bytes[16..20].copy_from_slice(&width.to_le_bytes());
        bytes[20..24].copy_from_slice(&(width * 4).to_le_bytes());
        bytes[28..32].copy_from_slice(&1u32.to_le_bytes());
        bytes[76..80].copy_from_slice(&32u32.to_le_bytes());
        bytes[80..84].copy_from_slice(&format_flags.to_le_bytes());
        bytes[84..88].copy_from_slice(&fourcc);
        bytes[88..92].copy_from_slice(&(if fourcc == [0; 4] { 32u32 } else { 0u32 }).to_le_bytes());
        bytes[92..96].copy_from_slice(&0x00ff_0000u32.to_le_bytes());
        bytes[96..100].copy_from_slice(&0x0000_ff00u32.to_le_bytes());
        bytes[100..104].copy_from_slice(&0x0000_00ffu32.to_le_bytes());
        bytes[104..108].copy_from_slice(&0xff00_0000u32.to_le_bytes());
        bytes
    }

    #[test]
    fn uncompressed_bgra32_round_trips() {
        let mut bytes = header(2, 1, 0x40 | DDPF_ALPHAPIXELS, [0; 4]); // DDPF_RGB | alpha.
        bytes.extend_from_slice(&[0, 0, 255, 255, 0, 255, 0, 128]);
        let image = decode_dds(&bytes).expect("decode");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(&image.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(&image.pixels[4..8], &[0, 255, 0, 128]);
    }

    #[test]
    fn uncompressed_24bit_uses_masks() {
        // 2x1, 24-bit BGRA-style masks but no alpha: bytes B,G,R.
        let mut bytes = header(2, 1, 0x40, [0; 4]); // DDPF_RGB, no alpha plane.
        bytes[88..92].copy_from_slice(&24u32.to_le_bytes());
        bytes[104..108].copy_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[10, 20, 30, 40, 50, 60]);
        let image = decode_dds(&bytes).expect("decode");
        assert_eq!(&image.pixels[..3], &[30, 20, 10]); // masks are BGRA-like.
        assert_eq!(image.pixels[3], 255);
        assert_eq!(&image.pixels[4..7], &[60, 50, 40]);
    }

    #[test]
    fn dxt1_palette_colors_decode() {
        // c0=0xF800 red, c1=0x07E0 green, all indices 0.
        let mut block = [0u8; 8];
        block[0] = 0x00;
        block[1] = 0xF8;
        block[2] = 0xE0;
        block[3] = 0x07;
        let out = decode_dxt1_block(&block).expect("decode");
        assert_eq!(&out[..3], &[255, 0, 0]);
        assert_eq!(out[3], 255);
        assert_eq!(&out[60..63], &[255, 0, 0]);
    }

    #[test]
    fn dxt3_alpha_nibbles_are_expanded() {
        let mut block = [0u8; 16];
        block[0] = 0x1f; // pixel 0 = 0xF, pixel 1 = 0x1.
        block[0] = 0x1f;
        let out = decode_dxt3_block(&block).expect("decode");
        assert_eq!(out[3], 0xf * 17);
        assert_eq!(out[7], 17); // 0x1 * 17.
        assert_eq!(out[11], 0);
    }

    #[test]
    fn dxt5_ramps_follow_the_spec() {
        // a0 > a1 -> 8-level ramp.
        let mut block = [0u8; 16];
        block[0] = 255;
        block[1] = 0;
        let out = decode_dxt5_block(&block).expect("decode");
        assert_eq!(out[3], 255); // index 0 -> a0.
        // a0 <= a1 -> 6 interpolants plus 0 and 255.
        let mut block = [0u8; 16];
        block[0] = 0;
        block[1] = 255;
        let out = decode_dxt5_block(&block).expect("decode");
        assert_eq!(out[3], 0);
        // Pixel 5, index 5 -> fifth interpolant ((1*a0+4*a1)/5 = 204).
        let bit = 5 * 3;
        let byte = 2 + bit / 8;
        let shift = bit % 8;
        block[byte] |= (5u16 << shift) as u8;
        block[byte + 1] = (5u16 >> (8 - shift)) as u8;
        let out = decode_dxt5_block(&block).expect("decode");
        assert_eq!(out[5 * 4 + 3], 204);
    }

    #[test]
    fn tiny_dxt3_2x2_image_decodes() {
        let mut bytes = header(2, 2, DDPF_FOURCC, *b"DXT3");
        bytes.extend_from_slice(&[0u8; 16]);
        let image = decode_dds(&bytes).expect("decode");
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.pixels.len(), 16);
    }

    #[test]
    fn rejects_malformed_headers_and_data() {
        assert_eq!(decode_dds(&[]), Err(DdsError("missing DDS header")));
        assert_eq!(decode_dds(&[0u8; 128]), Err(DdsError("missing DDS header")));
        let mut bytes = header(0, 1, 0x40, [0; 4]);
        bytes.extend_from_slice(&[0u8; 8]);
        assert_eq!(
            decode_dds(&bytes),
            Err(DdsError("invalid texture dimensions"))
        );
        let mut bytes = header(2, 2, DDPF_FOURCC, *b"NOPE");
        bytes.extend_from_slice(&[0u8; 8]);
        assert_eq!(
            decode_dds(&bytes),
            Err(DdsError("unsupported fourcc (only DXT1/3/5)"))
        );
        let bytes = header(2, 2, DDPF_FOURCC, *b"DXT1"); // no data at all
        assert_eq!(decode_dds(&bytes), Err(DdsError("truncated pixel data")));
    }
}
