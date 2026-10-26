//! Plaintext payloads (memo, vault) and their padding. See `docs/crypto.md#plaintext-payloads`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    Error, Result,
    keys::{AttachmentKey, PublicKey},
};

pub const MEMO_PAYLOAD_VERSION: u32 = 1;
pub const VAULT_PAYLOAD_VERSION: u32 = 1;
const PAD_BLOCK: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoPayload {
    pub v: u32,
    /// CommonMark. Images are referenced as `attachment:<id>`.
    pub body: String,
    /// Client clock at the time of the edit; display only.
    pub edited_at: i64,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
}

impl MemoPayload {
    pub fn new(body: impl Into<String>, edited_at: i64) -> Self {
        Self { v: MEMO_PAYLOAD_VERSION, body: body.into(), edited_at, attachments: Vec::new() }
    }

    /// `false` for payloads written by a newer client: show them read-only, never rewrite them.
    pub fn is_editable(&self) -> bool {
        self.v <= MEMO_PAYLOAD_VERSION
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub id: Uuid,
    pub key: AttachmentKey,
    pub mime: String,
    /// Plaintext size of the original, in bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub thumb: Option<ThumbRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbRef {
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultPayload {
    pub v: u32,
    #[serde(default)]
    pub contacts: BTreeMap<Uuid, Contact>,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl Default for VaultPayload {
    fn default() -> Self {
        Self { v: VAULT_PAYLOAD_VERSION, contacts: BTreeMap::new(), settings: serde_json::Map::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub username: String,
    pub public_key: PublicKey,
    pub pinned_at: i64,
    pub verified: bool,
}

pub(crate) fn encode_memo(payload: &MemoPayload) -> Result<Zeroizing<Vec<u8>>> {
    if !payload.is_editable() {
        return Err(Error::UnsupportedPayloadVersion(payload.v));
    }
    encode(payload)
}

/// Payloads from newer clients still decode as long as their shape is compatible; callers check
/// [`MemoPayload::is_editable`].
pub(crate) fn decode_memo(plaintext: &[u8]) -> Result<MemoPayload> {
    decode(plaintext, MEMO_PAYLOAD_VERSION, "memo payload")
}

pub(crate) fn encode_vault(payload: &VaultPayload) -> Result<Zeroizing<Vec<u8>>> {
    if payload.v > VAULT_PAYLOAD_VERSION {
        return Err(Error::UnsupportedPayloadVersion(payload.v));
    }
    encode(payload)
}

/// Unlike memos, a vault from a newer client is not decoded at all: it would be rewritten on the
/// next contact pin and lose data.
pub(crate) fn decode_vault(plaintext: &[u8]) -> Result<VaultPayload> {
    let vault: VaultPayload = decode(plaintext, VAULT_PAYLOAD_VERSION, "vault payload")?;
    if vault.v > VAULT_PAYLOAD_VERSION {
        return Err(Error::UnsupportedPayloadVersion(vault.v));
    }
    Ok(vault)
}

fn encode(payload: &impl Serialize) -> Result<Zeroizing<Vec<u8>>> {
    let json = Zeroizing::new(serde_json::to_vec(payload).expect("payload types always serialize"));
    pad(&json)
}

fn decode<T: DeserializeOwned>(plaintext: &[u8], supported: u32, what: &'static str) -> Result<T> {
    #[derive(Deserialize)]
    struct VersionOnly {
        v: u32,
    }
    let json = unpad(plaintext)?;
    let VersionOnly { v } = serde_json::from_slice(json).map_err(|_| Error::Malformed(what))?;
    serde_json::from_slice(json)
        .map_err(|_| if v > supported { Error::UnsupportedPayloadVersion(v) } else { Error::Malformed(what) })
}

/// `len:u32 || data || zeros`, padded to a multiple of 256 bytes.
fn pad(data: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let len = u32::try_from(data.len()).map_err(|_| Error::Malformed("payload too large"))?;
    let total = (4 + data.len()).div_ceil(PAD_BLOCK) * PAD_BLOCK;
    let mut out = Zeroizing::new(Vec::with_capacity(total));
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    out.resize(total, 0);
    Ok(out)
}

fn unpad(padded: &[u8]) -> Result<&[u8]> {
    if padded.len() < 4 || !padded.len().is_multiple_of(PAD_BLOCK) {
        return Err(Error::Malformed("padding"));
    }
    let (len, rest) = padded.split_at(4);
    let len = u32::from_be_bytes(len.try_into().expect("split at 4")) as usize;
    if len > rest.len() || rest[len..].iter().any(|&b| b != 0) {
        return Err(Error::Malformed("padding"));
    }
    Ok(&rest[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemoPayload {
        let mut payload = MemoPayload::new(
            "# Groceries\n- milk\n\n![](attachment:0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f)",
            1_761_480_000_000,
        );
        payload.attachments.push(AttachmentRef {
            id: "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f".parse().unwrap(),
            key: AttachmentKey::from_bytes(&[3; 32]).unwrap(),
            mime: "image/jpeg".into(),
            size: 2_412_331,
            width: 4000,
            height: 3000,
            thumb: Some(ThumbRef { mime: "image/webp".into(), width: 512, height: 384 }),
        });
        payload
    }

    #[test]
    fn memo_roundtrip_is_padded() {
        let encoded = encode_memo(&sample()).unwrap();
        assert_eq!(encoded.len() % 256, 0);
        assert_eq!(decode_memo(&encoded).unwrap(), sample());
    }

    #[test]
    fn padding_sizes() {
        assert_eq!(pad(&[]).unwrap().len(), 256);
        assert_eq!(pad(&[1; 252]).unwrap().len(), 256);
        assert_eq!(pad(&[1; 253]).unwrap().len(), 512);
    }

    #[test]
    fn unpad_rejects_bad_input() {
        let mut padded = pad(b"{}").unwrap();
        padded[100] = 1;
        assert!(unpad(&padded).is_err());
        assert!(unpad(&[0; 255]).is_err());
        let mut too_long = vec![0; 256];
        too_long[..4].copy_from_slice(&300u32.to_be_bytes());
        assert!(unpad(&too_long).is_err());
    }

    #[test]
    fn newer_memo_payload_decodes_read_only() {
        let json = br#"{"v":2,"body":"hi","edited_at":1,"attachments":[],"title":"new field"}"#;
        let payload = decode_memo(&pad(json).unwrap()).unwrap();
        assert!(!payload.is_editable());
        assert_eq!(payload.body, "hi");
        assert_eq!(encode_memo(&payload).unwrap_err(), Error::UnsupportedPayloadVersion(2));

        let incompatible = br#"{"v":2,"content":{"blocks":[]}}"#;
        assert_eq!(decode_memo(&pad(incompatible).unwrap()).unwrap_err(), Error::UnsupportedPayloadVersion(2));
        assert_eq!(decode_memo(&pad(br#"{"v":1}"#).unwrap()).unwrap_err(), Error::Malformed("memo payload"));
    }

    #[test]
    fn newer_vault_is_refused() {
        let json = br#"{"v":2,"contacts":{},"settings":{}}"#;
        assert_eq!(decode_vault(&pad(json).unwrap()).unwrap_err(), Error::UnsupportedPayloadVersion(2));
        let vault = VaultPayload::default();
        assert_eq!(decode_vault(&encode_vault(&vault).unwrap()).unwrap(), vault);
    }
}
