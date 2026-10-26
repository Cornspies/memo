//! Test vectors (`docs/test-vectors/v1.json`).
//!
//! `vectors_file_is_current` regenerates them from a seeded RNG and fails if the committed file
//! differs, so any change to a wire format is caught. Regenerate on purpose with
//! `MEMO_WRITE_VECTORS=1 cargo test -p memo-core vectors`.
//!
//! `vectors_verify` checks the file through the public API only, the same way the WASM and
//! UniFFI bindings do on their platforms.

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    content::{seal_attachment_with_rng, seal_memo_with_rng},
    encoding::b64,
    kdf::derive_password_keys,
    share::wrap_memo_key_with_rng,
    *,
};

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/test-vectors/v1.json");
const SEED: [u8; 32] = *b"memo/v1/test-vectors-seed-------";

#[derive(Debug, Serialize, Deserialize)]
struct Vectors {
    description: String,
    kdf: Vec<KdfVector>,
    recovery: Vec<RecoveryVector>,
    account: AccountVector,
    memo: Vec<MemoVector>,
    attachment: Vec<AttachmentVector>,
    key_wrap: Vec<KeyWrapVector>,
    fingerprint: Vec<FingerprintVector>,
}

#[derive(Debug, Serialize, Deserialize)]
struct KdfVector {
    password: String,
    #[serde(with = "b64")]
    salt: Vec<u8>,
    kdf: KdfParams,
    #[serde(with = "b64")]
    auth_key: Vec<u8>,
    #[serde(with = "b64")]
    master_key: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryVector {
    #[serde(with = "b64")]
    key: Vec<u8>,
    display: String,
    #[serde(with = "b64")]
    auth_key: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AccountVector {
    password: String,
    user_id: Uuid,
    #[serde(with = "b64")]
    kdf_salt: Vec<u8>,
    kdf: KdfParams,
    #[serde(with = "b64")]
    auth_key: Vec<u8>,
    recovery_key: String,
    #[serde(with = "b64")]
    recovery_auth_key: Vec<u8>,
    public_key: PublicKey,
    #[serde(with = "b64")]
    private_key: Vec<u8>,
    #[serde(with = "b64")]
    account_key: Vec<u8>,
    #[serde(with = "b64")]
    enc_account_key: Vec<u8>,
    #[serde(with = "b64")]
    enc_account_key_recovery: Vec<u8>,
    #[serde(with = "b64")]
    enc_private_key: Vec<u8>,
    #[serde(with = "b64")]
    vault: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MemoVector {
    #[serde(with = "b64")]
    memo_key: Vec<u8>,
    memo_id: Uuid,
    version: u64,
    key_version: u32,
    payload: MemoPayload,
    #[serde(with = "b64")]
    ciphertext: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AttachmentVector {
    #[serde(with = "b64")]
    key: Vec<u8>,
    attachment_id: Uuid,
    kind: AttachmentKind,
    #[serde(with = "b64")]
    plaintext: Vec<u8>,
    #[serde(with = "b64")]
    ciphertext: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyWrapVector {
    memo_id: Uuid,
    key_version: u32,
    owner_id: Uuid,
    #[serde(with = "b64")]
    owner_private_key: Vec<u8>,
    owner_public_key: PublicKey,
    member_id: Uuid,
    #[serde(with = "b64")]
    member_private_key: Vec<u8>,
    member_public_key: PublicKey,
    #[serde(with = "b64")]
    memo_key: Vec<u8>,
    #[serde(with = "b64")]
    wrapped_key: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FingerprintVector {
    user_id: Uuid,
    public_key: PublicKey,
    fingerprint: String,
}

fn id(n: u8) -> Uuid {
    // A valid UUIDv7 layout with fixed contents.
    Uuid::from_bytes([0x01, 0x92, 0xf3, 0xc4, 0x7a, 0x1b, 0x7c, n, 0x9e, 0x8f, 0x0a, 0x1b, 0x2c, 0x3d, 0x4e, n])
}

fn generate() -> Vectors {
    let rng = &mut ChaCha20Rng::from_seed(SEED);

    let kdf = ["correct horse battery staple", "caf\u{e9} \u{1f600} pässwörd"]
        .into_iter()
        .map(|password| {
            let salt = kdf::generate_salt_with_rng(rng);
            let keys = derive_password_keys(password, &salt, KdfParams::DEFAULT).unwrap();
            KdfVector {
                password: password.into(),
                salt: salt.to_vec(),
                kdf: KdfParams::DEFAULT,
                auth_key: keys.auth_key.as_bytes().to_vec(),
                master_key: keys.master_key.as_bytes().to_vec(),
            }
        })
        .collect();

    let recovery = (0..2)
        .map(|_| {
            let key = RecoveryKey::generate_with_rng(rng);
            RecoveryVector {
                key: key.as_bytes().to_vec(),
                display: key.to_display().to_string(),
                auth_key: key.auth_key().as_bytes().to_vec(),
            }
        })
        .collect();

    let password = "a long and memorable passphrase";
    let reg = account::register_with_rng(password, id(1), KdfParams::DEFAULT, rng).unwrap();
    let account = AccountVector {
        password: password.into(),
        user_id: reg.user_id,
        kdf_salt: reg.kdf_salt.to_vec(),
        kdf: reg.kdf,
        auth_key: reg.auth_key.as_bytes().to_vec(),
        recovery_key: reg.recovery_key.to_display().to_string(),
        recovery_auth_key: reg.recovery_auth_key.as_bytes().to_vec(),
        public_key: reg.public_key,
        private_key: reg.account.keypair.secret_bytes().to_vec(),
        account_key: reg.account.account_key.as_bytes().to_vec(),
        enc_account_key: reg.enc_account_key,
        enc_account_key_recovery: reg.enc_account_key_recovery,
        enc_private_key: reg.enc_private_key,
        vault: reg.vault,
    };

    let mut with_image =
        MemoPayload::new(format!("# Groceries\n- milk\n\n![](attachment:{})", id(3)), 1_761_480_000_000);
    with_image.attachments.push(AttachmentRef {
        id: id(3),
        key: AttachmentKey::generate_with_rng(rng),
        mime: "image/jpeg".into(),
        size: 2_412_331,
        width: 4000,
        height: 3000,
        thumb: Some(ThumbRef { mime: "image/webp".into(), width: 512, height: 384 }),
    });
    let memo = [
        (MemoPayload::new("", 0), 1, 1),
        (MemoPayload::new("Ünïcödé #tags and emoji \u{1f9e0}\n".repeat(20), 1_761_480_123_456), 7, 1),
        (with_image, 4_294_967_301, 3),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (payload, version, key_version))| {
        let memo_key = MemoKey::generate_with_rng(rng);
        let ctx = MemoContext { memo_id: id(10 + i as u8), version, key_version };
        MemoVector {
            memo_key: memo_key.as_bytes().to_vec(),
            memo_id: ctx.memo_id,
            version,
            key_version,
            ciphertext: seal_memo_with_rng(&memo_key, &ctx, &payload, rng).unwrap(),
            payload,
        }
    })
    .collect();

    let attachment = [
        (AttachmentKind::Original, b"\xff\xd8\xff\xe0 not really a jpeg".as_slice()),
        (AttachmentKind::Thumbnail, b"RIFF....WEBP"),
    ]
    .into_iter()
    .map(|(kind, plaintext)| {
        let key = AttachmentKey::generate_with_rng(rng);
        AttachmentVector {
            key: key.as_bytes().to_vec(),
            attachment_id: id(3),
            kind,
            plaintext: plaintext.to_vec(),
            ciphertext: seal_attachment_with_rng(&key, id(3), kind, plaintext, rng),
        }
    })
    .collect();

    let owner = UserKeyPair::generate_with_rng(rng);
    let member = UserKeyPair::generate_with_rng(rng);
    let key_wrap = [(id(1), &owner), (id(2), &member)]
        .into_iter()
        .map(|(member_id, member)| {
            let grant = KeyGrant { memo_id: id(12), key_version: 3, owner_id: id(1), member_id };
            let memo_key = MemoKey::generate_with_rng(rng);
            KeyWrapVector {
                memo_id: grant.memo_id,
                key_version: grant.key_version,
                owner_id: grant.owner_id,
                owner_private_key: owner.secret_bytes().to_vec(),
                owner_public_key: *owner.public_key(),
                member_id,
                member_private_key: member.secret_bytes().to_vec(),
                member_public_key: *member.public_key(),
                memo_key: memo_key.as_bytes().to_vec(),
                wrapped_key: wrap_memo_key_with_rng(&owner, member.public_key(), &grant, &memo_key, rng),
            }
        })
        .collect();

    let fingerprint = [(id(1), *owner.public_key()), (id(2), *member.public_key())]
        .into_iter()
        .map(|(user_id, public_key)| FingerprintVector {
            user_id,
            public_key,
            fingerprint: fingerprint(user_id, &public_key),
        })
        .collect();

    Vectors {
        description: "memo v1 test vectors, generated by crates/core/src/vectors.rs. See docs/crypto.md.".into(),
        kdf,
        recovery,
        account,
        memo,
        attachment,
        key_wrap,
        fingerprint,
    }
}

#[test]
fn vectors_file_is_current() {
    let generated = serde_json::to_string_pretty(&generate()).unwrap() + "\n";
    if std::env::var_os("MEMO_WRITE_VECTORS").is_some() {
        std::fs::create_dir_all(std::path::Path::new(PATH).parent().unwrap()).unwrap();
        std::fs::write(PATH, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(PATH).unwrap_or_default();
    assert!(
        committed == generated,
        "docs/test-vectors/v1.json is out of date. If a wire format changed on purpose, \
         regenerate with: MEMO_WRITE_VECTORS=1 cargo test -p memo-core vectors"
    );
}

#[test]
fn vectors_verify() {
    let v: Vectors = serde_json::from_str(&std::fs::read_to_string(PATH).expect("vectors file exists")).unwrap();

    for k in &v.kdf {
        let keys = derive_password_keys(&k.password, &k.salt.clone().try_into().unwrap(), k.kdf).unwrap();
        assert_eq!(keys.auth_key.as_bytes().as_slice(), k.auth_key, "kdf auth_key");
        assert_eq!(keys.master_key.as_bytes().as_slice(), k.master_key, "kdf master_key");
    }

    for r in &v.recovery {
        let key = RecoveryKey::parse(&r.display).unwrap();
        assert_eq!(key.as_bytes().as_slice(), r.key);
        assert_eq!(key.auth_key().as_bytes().as_slice(), r.auth_key);
    }

    let a = &v.account;
    let keys = derive_password_keys(&a.password, &a.kdf_salt.clone().try_into().unwrap(), a.kdf).unwrap();
    assert_eq!(keys.auth_key.as_bytes().as_slice(), a.auth_key);
    let unlocked = unlock(a.user_id, &keys.master_key, &a.public_key, &a.enc_account_key, &a.enc_private_key).unwrap();
    assert_eq!(unlocked.account_key.as_bytes().as_slice(), a.account_key);
    assert_eq!(unlocked.keypair.secret_bytes().as_slice(), a.private_key);
    let recovery_key = RecoveryKey::parse(&a.recovery_key).unwrap();
    assert_eq!(recovery_key.auth_key().as_bytes().as_slice(), a.recovery_auth_key);
    assert_eq!(
        recover_account_key(a.user_id, &recovery_key, &a.enc_account_key_recovery).unwrap(),
        unlocked.account_key
    );
    assert_eq!(open_vault(&unlocked.account_key, a.user_id, 1, &a.vault).unwrap(), VaultPayload::default());

    for m in &v.memo {
        let key = MemoKey::from_bytes(&m.memo_key).unwrap();
        let ctx = MemoContext { memo_id: m.memo_id, version: m.version, key_version: m.key_version };
        assert_eq!(open_memo(&key, &ctx, &m.ciphertext).unwrap(), m.payload);
    }

    for at in &v.attachment {
        let key = AttachmentKey::from_bytes(&at.key).unwrap();
        assert_eq!(open_attachment(&key, at.attachment_id, at.kind, &at.ciphertext).unwrap(), at.plaintext);
    }

    for w in &v.key_wrap {
        let member = UserKeyPair::from_secret_bytes(&w.member_private_key).unwrap();
        assert_eq!(member.public_key(), &w.member_public_key);
        let grant =
            KeyGrant { memo_id: w.memo_id, key_version: w.key_version, owner_id: w.owner_id, member_id: w.member_id };
        let memo_key = unwrap_memo_key(&member, &w.owner_public_key, &grant, &w.wrapped_key).unwrap();
        assert_eq!(memo_key.as_bytes().as_slice(), w.memo_key);
    }

    for f in &v.fingerprint {
        assert_eq!(fingerprint(f.user_id, &f.public_key), f.fingerprint);
    }
}
