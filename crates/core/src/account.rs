//! Account flows: registration, unlock after login, password change and recovery.
//! See `docs/crypto.md#account-flows`. Networking is the caller's job; these functions only
//! produce and consume the values the API sends.

use rand_core::CryptoRng;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    Error, Result,
    content::seal_vault_with_rng,
    envelope::{self, Purpose},
    kdf::{self, KdfParams, Salt},
    keys::{AccountKey, AuthKey, MasterKey, PublicKey, RecoveryAuthKey, UserKeyPair, os_rng},
    payload::VaultPayload,
    recovery::RecoveryKey,
};

/// The keys of a logged-in user. Native clients store `account_key` and the private key in the
/// OS keystore and rebuild this with [`UnlockedAccount::from_parts`].
#[derive(Clone, Debug)]
pub struct UnlockedAccount {
    pub user_id: Uuid,
    pub account_key: AccountKey,
    pub keypair: UserKeyPair,
}

/// Everything `POST /auth/register` needs, plus what stays on the device.
#[derive(Debug)]
pub struct Registration {
    pub user_id: Uuid,
    pub kdf_salt: Salt,
    pub kdf: KdfParams,
    pub auth_key: AuthKey,
    pub recovery_auth_key: RecoveryAuthKey,
    pub public_key: PublicKey,
    pub enc_account_key: Vec<u8>,
    pub enc_account_key_recovery: Vec<u8>,
    pub enc_private_key: Vec<u8>,
    /// Version 1, empty.
    pub vault: Vec<u8>,
    /// Show once; never send.
    pub recovery_key: RecoveryKey,
    pub account: UnlockedAccount,
}

/// Body of `POST /auth/password` (and the password half of `/auth/recover/complete`).
#[derive(Debug)]
pub struct PasswordChange {
    pub kdf_salt: Salt,
    pub kdf: KdfParams,
    pub auth_key: AuthKey,
    pub enc_account_key: Vec<u8>,
}

/// The recovery half of `/auth/recover/complete`.
#[derive(Debug)]
pub struct RecoveryRotation {
    /// Show once; never send.
    pub recovery_key: RecoveryKey,
    pub recovery_auth_key: RecoveryAuthKey,
    pub enc_account_key_recovery: Vec<u8>,
}

/// Creates a new account. Enforces the password policy.
pub fn register(password: &str) -> Result<Registration> {
    register_with_rng(password, Uuid::now_v7(), KdfParams::DEFAULT, &mut os_rng())
}

pub(crate) fn register_with_rng(
    password: &str,
    user_id: Uuid,
    kdf: KdfParams,
    rng: &mut impl CryptoRng,
) -> Result<Registration> {
    kdf::check_password_policy(password)?;
    let account = UnlockedAccount {
        user_id,
        account_key: AccountKey::generate_with_rng(rng),
        keypair: UserKeyPair::generate_with_rng(rng),
    };
    let password_change = account.password_material(password, kdf, rng)?;
    let recovery = account.recovery_material(rng);
    let enc_private_key = envelope::seal(
        account.account_key.as_bytes(),
        Purpose::PrivateKey { user_id },
        &*account.keypair.secret_bytes(),
        rng,
    );
    let vault = seal_vault_with_rng(&account.account_key, user_id, 1, &VaultPayload::default(), rng)?;
    Ok(Registration {
        user_id,
        kdf_salt: password_change.kdf_salt,
        kdf: password_change.kdf,
        auth_key: password_change.auth_key,
        recovery_auth_key: recovery.recovery_auth_key,
        public_key: *account.keypair.public_key(),
        enc_account_key: password_change.enc_account_key,
        enc_account_key_recovery: recovery.enc_account_key_recovery,
        enc_private_key,
        vault,
        recovery_key: recovery.recovery_key,
        account,
    })
}

/// After `POST /auth/login`: decrypts the account key with the password-derived master key,
/// then the private key, and checks it against the public key the server returned.
pub fn unlock(
    user_id: Uuid,
    master_key: &MasterKey,
    public_key: &PublicKey,
    enc_account_key: &[u8],
    enc_private_key: &[u8],
) -> Result<UnlockedAccount> {
    let account_key = open_key(master_key.as_bytes(), Purpose::AccountKey { user_id }, enc_account_key)?;
    unlock_with_account_key(user_id, account_key, public_key, enc_private_key)
}

/// After `POST /auth/recover`: recovers the account key with the recovery key.
pub fn recover_account_key(
    user_id: Uuid,
    recovery_key: &RecoveryKey,
    enc_account_key_recovery: &[u8],
) -> Result<AccountKey> {
    open_key(recovery_key.wrap_key().as_bytes(), Purpose::AccountKeyRecovery { user_id }, enc_account_key_recovery)
}

pub fn unlock_with_account_key(
    user_id: Uuid,
    account_key: AccountKey,
    public_key: &PublicKey,
    enc_private_key: &[u8],
) -> Result<UnlockedAccount> {
    let secret =
        Zeroizing::new(envelope::open(account_key.as_bytes(), Purpose::PrivateKey { user_id }, enc_private_key)?);
    let keypair = UserKeyPair::from_secret_bytes(&secret)?;
    if keypair.public_key() != public_key {
        return Err(Error::KeyMismatch);
    }
    Ok(UnlockedAccount { user_id, account_key, keypair })
}

fn open_key(wrapping_key: &[u8; 32], purpose: Purpose, ciphertext: &[u8]) -> Result<AccountKey> {
    let bytes = Zeroizing::new(envelope::open(wrapping_key, purpose, ciphertext)?);
    AccountKey::from_bytes(&bytes)
}

impl UnlockedAccount {
    /// Rebuilds keys from the OS keystore. The public key is recomputed from the private key.
    pub fn from_parts(user_id: Uuid, account_key: AccountKey, private_key: &[u8]) -> Result<Self> {
        Ok(Self { user_id, account_key, keypair: UserKeyPair::from_secret_bytes(private_key)? })
    }

    /// New salt, new password: only the account key gets re-wrapped. Enforces the password policy.
    pub fn change_password(&self, new_password: &str) -> Result<PasswordChange> {
        kdf::check_password_policy(new_password)?;
        self.password_material(new_password, KdfParams::DEFAULT, &mut os_rng())
    }

    /// Generates a replacement recovery key (after a recovery, or on request).
    pub fn new_recovery_key(&self) -> RecoveryRotation {
        self.recovery_material(&mut os_rng())
    }

    fn password_material(&self, password: &str, kdf: KdfParams, rng: &mut impl CryptoRng) -> Result<PasswordChange> {
        let kdf_salt = kdf::generate_salt_with_rng(rng);
        let keys = kdf::derive_password_keys(password, &kdf_salt, kdf)?;
        let enc_account_key = envelope::seal(
            keys.master_key.as_bytes(),
            Purpose::AccountKey { user_id: self.user_id },
            self.account_key.as_bytes(),
            rng,
        );
        Ok(PasswordChange { kdf_salt, kdf, auth_key: keys.auth_key, enc_account_key })
    }

    fn recovery_material(&self, rng: &mut impl CryptoRng) -> RecoveryRotation {
        let recovery_key = RecoveryKey::generate_with_rng(rng);
        let enc_account_key_recovery = envelope::seal(
            recovery_key.wrap_key().as_bytes(),
            Purpose::AccountKeyRecovery { user_id: self.user_id },
            self.account_key.as_bytes(),
            rng,
        );
        RecoveryRotation { recovery_auth_key: recovery_key.auth_key(), recovery_key, enc_account_key_recovery }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{content::open_vault, kdf::derive_password_keys};

    const PASSWORD: &str = "correct horse battery staple";

    #[test]
    fn register_then_login() {
        let reg = register(PASSWORD).unwrap();
        let keys = derive_password_keys(PASSWORD, &reg.kdf_salt, reg.kdf).unwrap();
        assert_eq!(keys.auth_key, reg.auth_key);
        let unlocked =
            unlock(reg.user_id, &keys.master_key, &reg.public_key, &reg.enc_account_key, &reg.enc_private_key).unwrap();
        assert_eq!(unlocked.account_key, reg.account.account_key);
        assert_eq!(open_vault(&unlocked.account_key, reg.user_id, 1, &reg.vault).unwrap(), VaultPayload::default());
    }

    #[test]
    fn wrong_password_fails() {
        let reg = register(PASSWORD).unwrap();
        let keys = derive_password_keys("incorrect horse battery staple", &reg.kdf_salt, reg.kdf).unwrap();
        assert_ne!(keys.auth_key, reg.auth_key);
        let result = unlock(reg.user_id, &keys.master_key, &reg.public_key, &reg.enc_account_key, &reg.enc_private_key);
        assert_eq!(result.unwrap_err(), Error::Decrypt);
    }

    #[test]
    fn substituted_public_key_is_detected() {
        let reg = register(PASSWORD).unwrap();
        let keys = derive_password_keys(PASSWORD, &reg.kdf_salt, reg.kdf).unwrap();
        let other = *UserKeyPair::generate().public_key();
        let result = unlock(reg.user_id, &keys.master_key, &other, &reg.enc_account_key, &reg.enc_private_key);
        assert_eq!(result.unwrap_err(), Error::KeyMismatch);
    }

    #[test]
    fn recovery_flow() {
        let reg = register(PASSWORD).unwrap();
        let typed = RecoveryKey::parse(&reg.recovery_key.to_display()).unwrap();
        assert_eq!(typed.auth_key(), reg.recovery_auth_key);
        let account_key = recover_account_key(reg.user_id, &typed, &reg.enc_account_key_recovery).unwrap();
        let account = unlock_with_account_key(reg.user_id, account_key, &reg.public_key, &reg.enc_private_key).unwrap();

        let change = account.change_password("a brand new password").unwrap();
        let rotation = account.new_recovery_key();
        let keys = derive_password_keys("a brand new password", &change.kdf_salt, change.kdf).unwrap();
        let relogged =
            unlock(reg.user_id, &keys.master_key, &reg.public_key, &change.enc_account_key, &reg.enc_private_key)
                .unwrap();
        assert_eq!(relogged.account_key, reg.account.account_key);
        assert!(recover_account_key(reg.user_id, &rotation.recovery_key, &rotation.enc_account_key_recovery).is_ok());
        assert!(recover_account_key(reg.user_id, &typed, &rotation.enc_account_key_recovery).is_err());
    }

    #[test]
    fn policy_applies_to_new_passwords_only() {
        assert_eq!(register("too short").unwrap_err(), Error::PasswordTooShort);
        let reg = register(PASSWORD).unwrap();
        assert_eq!(reg.account.change_password("short").unwrap_err(), Error::PasswordTooShort);
    }

    #[test]
    fn keystore_roundtrip() {
        let reg = register(PASSWORD).unwrap();
        let restored = UnlockedAccount::from_parts(
            reg.user_id,
            reg.account.account_key.clone(),
            &*reg.account.keypair.secret_bytes(),
        )
        .unwrap();
        assert_eq!(restored.keypair.public_key(), &reg.public_key);
    }
}
