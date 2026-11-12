//! Image checks before encryption (`docs/crypto.md#attachments`): the type comes from the magic
//! bytes, metadata is removed without re-encoding, and the EXIF orientation is reported so the client
//! can re-encode with the rotation applied when it is not 1.

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    Jpeg,
    Png,
    Webp,
    Gif,
}

impl ImageType {
    /// Detects an allowed type from the first bytes. Anything else is `None`.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        [Self::Jpeg, Self::Png, Self::Webp, Self::Gif].into_iter().find(|t| t.mime() == mime)
    }
}

/// Whether decrypted bytes may be rendered inline as `mime` (allowlisted, and the bytes agree).
pub fn renderable_as(bytes: &[u8], mime: &str) -> bool {
    ImageType::sniff(bytes).is_some_and(|t| t.mime() == mime)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanImage {
    pub kind: ImageType,
    pub width: u32,
    pub height: u32,
    /// EXIF orientation, 1 to 8. When it is not 1 the client re-encodes with the rotation applied
    /// (which also drops metadata) instead of using `bytes`.
    pub orientation: u16,
    /// The image without EXIF, XMP, IPTC, comments or PNG text chunks.
    pub bytes: Vec<u8>,
}

/// Checks the type, reads the dimensions and orientation, and strips metadata.
pub fn clean_image(bytes: &[u8]) -> Result<CleanImage> {
    let kind = ImageType::sniff(bytes).ok_or(Error::Malformed("image: not a JPEG, PNG, WebP or GIF"))?;
    let (bytes, size, orientation) = match kind {
        ImageType::Jpeg => jpeg(bytes),
        ImageType::Png => png(bytes),
        ImageType::Webp => webp(bytes),
        ImageType::Gif => gif(bytes),
    }
    .ok_or(Error::Malformed("image"))?;
    let (width, height) = size.filter(|&(w, h)| w > 0 && h > 0).ok_or(Error::Malformed("image: no dimensions"))?;
    Ok(CleanImage { kind, width, height, orientation: orientation.filter(|o| (1..=8).contains(o)).unwrap_or(1), bytes })
}

type Cleaned = (Vec<u8>, Option<(u32, u32)>, Option<u16>);

/// `len` bytes at `at`, or `None` if they are not all there. Offsets come from the file, so the
/// sum may not even fit (32-bit targets).
fn bytes(b: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    b.get(at..at.checked_add(len)?)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes(b, at, 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes(b, at, 4)?.try_into().ok()?))
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes(b, at, 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes(b, at, 4)?.try_into().ok()?))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = bytes(b, at, 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

/// The orientation tag (0x0112) in IFD0 of a TIFF structure (the body of an EXIF block).
fn exif_orientation(tiff: &[u8]) -> Option<u16> {
    let little = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |at| if little { le16(tiff, at) } else { be16(tiff, at) };
    let u32_at = |at| if little { le32(tiff, at) } else { be32(tiff, at) };
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count.min(256)).find_map(|i| {
        let entry = ifd.checked_add(2 + i * 12)?;
        // Type 3 is SHORT; the value sits in the first two bytes of the value field.
        (u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3).then(|| u16_at(entry + 8)).flatten()
    })
}

fn jpeg(b: &[u8]) -> Option<Cleaned> {
    let mut out = b[..2].to_vec();
    let (mut size, mut orientation) = (None, None);
    let mut at = 2;
    loop {
        // Fill bytes before a marker are allowed.
        while b.get(at)? == &0xFF && b.get(at + 1)? == &0xFF {
            at += 1;
        }
        if *b.get(at)? != 0xFF {
            return None;
        }
        let marker = *b.get(at + 1)?;
        if marker == 0xD9 {
            out.extend_from_slice(&b[at..at + 2]);
            return Some((out, size, orientation));
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            out.extend_from_slice(&b[at..at + 2]);
            at += 2;
            continue;
        }
        // The length counts its own two bytes.
        let len = be16(b, at + 2)? as usize;
        let segment = bytes(b, at, 2 + len)?;
        let data = segment.get(4..)?;
        let keep = match marker {
            // APP1: EXIF or XMP. Read the orientation, then drop it.
            0xE1 => {
                if let Some(tiff) = data.strip_prefix(b"Exif\0\0") {
                    orientation = orientation.or_else(|| exif_orientation(tiff));
                }
                false
            }
            // APP2 keeps only the ICC colour profile; APP0 (JFIF) and APP14 (Adobe) affect decoding.
            0xE2 => data.starts_with(b"ICC_PROFILE\0"),
            0xE0 | 0xEE => true,
            0xE3..=0xEF | 0xFE => false,
            // Start of frame (not DHT, JPG or DAC): height, then width.
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                size = size.or(Some((u32::from(be16(data, 3)?), u32::from(be16(data, 1)?))));
                true
            }
            _ => true,
        };
        if keep {
            out.extend_from_slice(segment);
        }
        at += 2 + len;
        // Start of scan: the rest is entropy-coded data and later segments of a progressive image.
        if marker == 0xDA {
            out.extend_from_slice(&b[at..]);
            return Some((out, size, orientation));
        }
    }
}

fn png(b: &[u8]) -> Option<Cleaned> {
    let mut out = b[..8].to_vec();
    let (mut size, mut orientation) = (None, None);
    let mut at = 8;
    while at < b.len() {
        let len = be32(b, at)? as usize;
        let kind = bytes(b, at + 4, 4)?;
        let chunk = bytes(b, at, len.checked_add(12)?)?;
        let data = &chunk[8..8 + len];
        match kind {
            b"IHDR" => size = Some((be32(data, 0)?, be32(data, 4)?)),
            b"eXIf" => orientation = exif_orientation(data),
            _ => {}
        }
        if !matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME") {
            out.extend_from_slice(chunk);
        }
        at += 12 + len;
        if kind == b"IEND" {
            break;
        }
    }
    Some((out, size, orientation))
}

fn webp(b: &[u8]) -> Option<Cleaned> {
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let (mut size, mut orientation) = (None, None);
    let end = (le32(b, 4)? as usize).checked_add(8)?.min(b.len());
    let mut at = 12;
    while at + 8 <= end {
        let kind = bytes(b, at, 4)?;
        let len = le32(b, at + 4)? as usize;
        let padded = len.checked_add(len & 1)?;
        let chunk = b.get(at..at.checked_add(8)?.checked_add(padded)?.min(end))?;
        // Inside the chunk, so nothing past the RIFF length is read.
        let data = bytes(chunk, 8, len)?;
        match kind {
            b"VP8X" => size = Some((le24(data, 4)? + 1, le24(data, 7)? + 1)),
            b"VP8 " if size.is_none() => {
                size = Some((u32::from(le16(data, 6)? & 0x3FFF), u32::from(le16(data, 8)? & 0x3FFF)))
            }
            b"VP8L" if size.is_none() => {
                let bits = le32(data, 1)?;
                size = Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1));
            }
            b"EXIF" => orientation = exif_orientation(data.strip_prefix(b"Exif\0\0").unwrap_or(data)),
            _ => {}
        }
        match kind {
            b"EXIF" | b"XMP " => {}
            b"VP8X" => {
                let mut chunk = chunk.to_vec();
                chunk[8] &= !(0x08 | 0x04); // EXIF and XMP flags
                out.extend_from_slice(&chunk);
            }
            _ => out.extend_from_slice(chunk),
        }
        at += 8 + padded;
    }
    let riff_len = u32::try_from(out.len() - 8).ok()?;
    out[4..8].copy_from_slice(&riff_len.to_le_bytes());
    Some((out, size, orientation))
}

fn gif(b: &[u8]) -> Option<Cleaned> {
    let size = Some((u32::from(le16(b, 6)?), u32::from(le16(b, 8)?)));
    let color_table = |flags: u8| if flags & 0x80 != 0 { 3 << ((flags & 7) + 1) } else { 0 };
    let mut at = 13 + color_table(*b.get(10)?);
    let mut out = b.get(..at)?.to_vec();
    // Data sub-blocks: length-prefixed, ending with a zero length. Returns the end offset.
    let sub_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let len = *b.get(at)? as usize;
            at += 1 + len;
            if len == 0 {
                return Some(at);
            }
        }
    };
    loop {
        match *b.get(at)? {
            0x3B => {
                out.push(0x3B);
                return Some((out, size, None));
            }
            0x2C => {
                let start = at;
                at += 10 + color_table(*b.get(at + 9)?);
                let end = sub_blocks(at + 1)?;
                out.extend_from_slice(b.get(start..end)?);
                at = end;
            }
            0x21 => {
                let label = *b.get(at + 1)?;
                let end = sub_blocks(at + 2)?;
                // Keep graphic control (frame timing, transparency) and the looping extension.
                let looping = label == 0xFF && b.get(at + 3..at + 14).is_some_and(|id| id == b"NETSCAPE2.0");
                if label == 0xF9 || label == 0x01 || looping {
                    out.extend_from_slice(b.get(at..end)?);
                }
                at = end;
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_images_are_errors() {
        // A JPEG segment shorter than its own length field.
        assert!(clean_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00]).is_err());
        assert!(clean_image(&[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x01, 0x00]).is_err());
        // A PNG chunk length that only fits the buffer once the sum wraps (32-bit targets).
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFC]);
        png.extend_from_slice(b"IHDR\0\0\0\0\0\0\0\0");
        assert!(clean_image(&png).is_err());
        // A WebP whose RIFF length ends inside the VP8X chunk.
        let mut webp = b"RIFF\x0c\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        webp.extend_from_slice(&[0; 10]);
        assert!(clean_image(&webp).is_err());
        let mut webp = b"RIFF\xff\xff\xff\xffWEBPVP8X\xff\xff\xff\xff".to_vec();
        webp.extend_from_slice(&[0; 10]);
        assert!(clean_image(&webp).is_err());
        // Truncated everything.
        for start in [&b"\xFF\xD8\xFF"[..], b"\x89PNG\r\n\x1a\n", b"RIFF\0\0\0\0WEBP", b"GIF89a"] {
            assert!(clean_image(start).is_err());
        }
    }

    fn exif(orientation: u16) -> Vec<u8> {
        let mut tiff = b"MM\0*\0\0\0\x08\0\x01".to_vec();
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1]);
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        tiff
    }

    fn jpeg_segment(marker: u8, data: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, marker];
        s.extend_from_slice(&(data.len() as u16 + 2).to_be_bytes());
        s.extend_from_slice(data);
        s
    }

    #[test]
    fn jpeg_drops_exif_and_comments_but_keeps_the_image() {
        let app0 = jpeg_segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        let sof = jpeg_segment(0xC0, &[8, 0, 30, 0, 40, 1, 1, 0x11, 0]);
        let scan = [jpeg_segment(0xDA, &[1, 1, 0, 0, 63, 0]), vec![0x12, 0x34, 0xFF, 0x00, 0xFF, 0xD9]].concat();
        let input = [
            vec![0xFF, 0xD8],
            app0.clone(),
            jpeg_segment(0xE1, &[b"Exif\0\0".as_slice(), &exif(6)].concat()),
            jpeg_segment(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>"),
            jpeg_segment(0xE2, b"ICC_PROFILE\0\x01\x01data"),
            jpeg_segment(0xED, b"Photoshop 3.0\0IPTC"),
            jpeg_segment(0xFE, b"a comment"),
            sof.clone(),
            scan.clone(),
        ]
        .concat();
        let clean = clean_image(&input).unwrap();
        assert_eq!((clean.kind, clean.width, clean.height, clean.orientation), (ImageType::Jpeg, 40, 30, 6));
        let expected = [vec![0xFF, 0xD8], app0, jpeg_segment(0xE2, b"ICC_PROFILE\0\x01\x01data"), sof, scan].concat();
        assert_eq!(clean.bytes, expected);
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        // CRCs are not checked here; decoders check them, and stripping copies them unchanged.
        [&(data.len() as u32).to_be_bytes()[..], kind, data, &[0; 4]].concat()
    }

    #[test]
    fn png_drops_text_and_exif_chunks() {
        let ihdr = png_chunk(b"IHDR", &[0, 0, 1, 0, 0, 0, 0, 200, 8, 6, 0, 0, 0]);
        let idat = png_chunk(b"IDAT", &[1, 2, 3]);
        let iend = png_chunk(b"IEND", &[]);
        let input = [
            b"\x89PNG\r\n\x1a\n".to_vec(),
            ihdr.clone(),
            png_chunk(b"tEXt", b"Author\0me"),
            png_chunk(b"eXIf", &exif(8)),
            idat.clone(),
            png_chunk(b"tIME", &[0; 7]),
            iend.clone(),
        ]
        .concat();
        let clean = clean_image(&input).unwrap();
        assert_eq!((clean.width, clean.height, clean.orientation), (256, 200, 8));
        assert_eq!(clean.bytes, [b"\x89PNG\r\n\x1a\n".to_vec(), ihdr, idat, iend].concat());
    }

    fn riff_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = [&kind[..], &(data.len() as u32).to_le_bytes(), data].concat();
        if data.len() % 2 == 1 {
            c.push(0);
        }
        c
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body = chunks.concat();
        [b"RIFF".as_slice(), &(body.len() as u32 + 4).to_le_bytes(), b"WEBP", &body].concat()
    }

    #[test]
    fn webp_drops_exif_and_xmp_and_clears_their_flags() {
        let vp8x = |flags: u8| riff_chunk(b"VP8X", &[flags, 0, 0, 0, 99, 0, 0, 49, 0, 0]);
        let image = riff_chunk(b"VP8L", &[0x2F, 0, 0, 0, 0, 1]);
        let input = riff(&[vp8x(0x0C), image.clone(), riff_chunk(b"EXIF", &exif(3)), riff_chunk(b"XMP ", b"<x/>")]);
        let clean = clean_image(&input).unwrap();
        assert_eq!((clean.width, clean.height, clean.orientation), (100, 50, 3));
        assert_eq!(clean.bytes, riff(&[vp8x(0), image]));
    }

    #[test]
    fn gif_drops_comments_and_unknown_application_blocks() {
        let header = [b"GIF89a".as_slice(), &[3, 0, 2, 0, 0x80, 0, 0], &[0; 6]].concat();
        let looping = [&[0x21, 0xFF, 11][..], b"NETSCAPE2.0", &[3, 1, 0, 0, 0]].concat();
        let control = vec![0x21, 0xF9, 4, 0, 10, 0, 0, 0];
        let frame = vec![0x2C, 0, 0, 0, 0, 3, 0, 2, 0, 0, 2, 2, 0x44, 0x01, 0];
        let xmp = [&[0x21, 0xFF, 11][..], b"XMP DataXMP", &[2, b'<', b'>', 0]].concat();
        let comment = vec![0x21, 0xFE, 2, b'h', b'i', 0];
        let input =
            [header.clone(), looping.clone(), comment, xmp, control.clone(), frame.clone(), vec![0x3B]].concat();
        let clean = clean_image(&input).unwrap();
        assert_eq!((clean.width, clean.height, clean.orientation), (3, 2, 1));
        assert_eq!(clean.bytes, [header, looping, control, frame, vec![0x3B]].concat());
    }

    #[test]
    fn rejects_other_types_and_truncated_files() {
        assert!(clean_image(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(clean_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]).is_err());
        assert!(renderable_as(b"GIF89a....", "image/gif"));
        assert!(!renderable_as(b"GIF89a....", "image/png"));
        assert_eq!(ImageType::from_mime("image/webp"), Some(ImageType::Webp));
    }
}
