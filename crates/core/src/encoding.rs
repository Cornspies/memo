//! Wire encodings: base64url (no padding) for binary values in JSON, Crockford Base32 for
//! human-facing strings.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::{Error, Result};

pub fn b64_encode(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s).map_err(|_| Error::Malformed("base64"))
}

/// `#[serde(with = "b64")]` for `Vec<u8>` fields.
pub(crate) mod b64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::b64_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        super::b64_decode(&s).map_err(D::Error::custom)
    }
}

/// `#[serde(with = "b64_opt", default, skip_serializing_if = "Option::is_none")]` for `Option<Vec<u8>>`.
pub(crate) mod b64_opt {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => s.serialize_str(&super::b64_encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(d)?.map(|s| super::b64_decode(&s).map_err(D::Error::custom)).transpose()
    }
}

/// `#[serde(with = "b64_array")]` for `[u8; N]` fields.
pub(crate) mod b64_array {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer, const N: usize>(bytes: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::b64_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<[u8; N], D::Error> {
        let bytes = super::b64_decode(&String::deserialize(d)?).map_err(D::Error::custom)?;
        bytes.try_into().map_err(|_| D::Error::custom("wrong length"))
    }
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub(crate) fn crockford_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(CROCKFORD[((buf >> bits) & 31) as usize] as char);
        }
        buf &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(CROCKFORD[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Case-insensitive; skips whitespace and `-`; maps `I`/`L` to `1` and `O` to `0`.
/// Returns `None` on invalid characters or non-zero trailing bits.
pub(crate) fn crockford_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars() {
        let value = match c.to_ascii_uppercase() {
            '-' => continue,
            c if c.is_whitespace() => continue,
            'O' => 0,
            'I' | 'L' => 1,
            c => CROCKFORD.iter().position(|&a| char::from(a) == c)? as u32,
        };
        buf = (buf << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    (buf == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crockford_roundtrip() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(crockford_decode(&crockford_encode(&bytes)).unwrap(), bytes);
        }
    }

    #[test]
    fn crockford_lenient_input() {
        let encoded = crockford_encode(&[0x00, 0x44, 0x32, 0x14, 0xc7]);
        assert_eq!(encoded, "01234567");
        assert_eq!(crockford_decode("ol-23 4567"), crockford_decode(&encoded));
        assert_eq!(crockford_decode("OI234567"), crockford_decode(&encoded));
        assert_eq!(crockford_decode("U000"), None);
    }

    #[test]
    fn b64_is_url_safe_unpadded() {
        assert_eq!(b64_encode([0xfb, 0xff]), "-_8");
        assert_eq!(b64_decode("-_8").unwrap(), vec![0xfb, 0xff]);
        assert!(b64_decode("-_8=").is_err());
    }
}
