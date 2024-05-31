use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{
    ed25519::signature::SignerMut, Signature, SigningKey, Verifier, VerifyingKey, KEYPAIR_LENGTH,
    PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PublicKeyError {
    #[error("the data for the public key isn't long enough!")]
    PublicKeyTooShort,
    #[error("the data for the signature isn't long enough!")]
    SignatureTooShort,
    #[error("the public key string is in an unexpected format!")]
    UnexpectedFormat,
    #[error("unable to decode data for key")]
    UnableToDecode(#[from] base64::DecodeSliceError),
    #[error("unable to read public key data")]
    UnableToReadKey(#[from] ed25519_dalek::SignatureError),
    #[error("this key already exists in the keychain!")]
    KeyAlreadyInKeychain,
}

pub struct NixStylePublicKey {
    name: String,
    key: VerifyingKey,
}

impl NixStylePublicKey {
    /// Nix stores keys in the format `<name>:<base64str>`, where `<name>` is the name of the key as used by the cache, and `<base64str>` is a base64-encoded string of the bytes of the key.
    pub fn from_nix_format(s: &str) -> Result<Self, PublicKeyError> {
        if let [name, base64str] = s.split(":").collect::<Vec<_>>()[..] {
            let mut key_bytes = [0u8; PUBLIC_KEY_LENGTH];
            let bytes_written = STANDARD.decode_slice(base64str, &mut key_bytes)?;

            if bytes_written < key_bytes.len() {
                return Err(PublicKeyError::PublicKeyTooShort);
            }

            Ok(Self {
                name: name.to_string(),
                key: VerifyingKey::from_bytes(&key_bytes)?,
            })
        } else {
            Err(PublicKeyError::UnexpectedFormat)
        }
    }
}

#[derive(Error, Debug)]
pub enum PrivateKeyError {
    #[error("the data for the private key isn't long enough!")]
    PrivateKeyTooShort,
    #[error("the private key string is in an unexpected format!")]
    UnexpectedFormat,
    #[error("unable to decode data for key")]
    UnableToDecode(#[from] base64::DecodeSliceError),
    #[error("unable to read private key data")]
    UnableToReadKey(#[from] ed25519_dalek::SignatureError),
}

pub struct NixStylePrivateKey {
    name: String,
    key: SigningKey,
}

impl NixStylePrivateKey {
    /// Nix stores keys in the format `<name>:<base64str>`, where `<name>` is the name of the key as used by the cache, and `<base64str>` is a base64-encoded string of the bytes of the key.
    pub fn from_nix_format(s: &str) -> Result<Self, PrivateKeyError> {
        if let [name, base64str] = s.split(":").collect::<Vec<_>>()[..] {
            let mut key_bytes = [0u8; KEYPAIR_LENGTH];
            let bytes_written = STANDARD.decode_slice(base64str, &mut key_bytes)?;

            if bytes_written < key_bytes.len() {
                return Err(PrivateKeyError::PrivateKeyTooShort);
            }

            Ok(Self {
                name: name.to_string(),
                key: SigningKey::from_keypair_bytes(&key_bytes)?,
            })
        } else {
            Err(PrivateKeyError::UnexpectedFormat)
        }
    }

    pub fn sign_to_base64(&mut self, data: &[u8]) -> Result<String, PrivateKeyError> {
        let signature = self.key.sign(data);
        Ok(STANDARD.encode::<[u8; 64]>(signature.into()))
    }

    pub fn public_key_nix_format(&self) -> String {
        let pk = self.key.verifying_key();
        let pk_encoded = STANDARD.encode(pk.as_bytes());
        format!("{}:{}", self.name, pk_encoded)
    }
}

pub struct PublicKeychain {
    keys: HashMap<String, NixStylePublicKey>,
}

impl PublicKeychain {
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
        }
    }

    pub fn with_known_keys() -> Result<Self, PublicKeyError> {
        let mut this = Self::new();
        let nixos_key = NixStylePublicKey::from_nix_format(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=",
        )?;
        this.add_key(nixos_key)?;

        Ok(this)
    }

    pub fn add_key(&mut self, key: NixStylePublicKey) -> Result<(), PublicKeyError> {
        if self.keys.contains_key(&key.name) {
            Err(PublicKeyError::KeyAlreadyInKeychain)
        } else {
            self.keys.insert(key.name.clone(), key);
            Ok(())
        }
    }

    pub fn verify(
        &self,
        key_name: &str,
        data: &[u8],
        signature_base64: &[u8],
    ) -> Result<bool, PublicKeyError> {
        if let Some(key) = self.keys.get(key_name) {
            let signature = signature_from_base64(signature_base64)?;
            Ok(key.key.verify(data, &signature).is_ok())
        } else {
            Ok(false)
        }
    }

    pub fn verify_any(&self, data: &[u8], signature_base64: &[u8]) -> Result<bool, PublicKeyError> {
        let signature = signature_from_base64(signature_base64)?;

        for key in self.keys.values() {
            if key.key.verify(data, &signature).is_ok() {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

fn signature_from_base64(data: &[u8]) -> Result<Signature, PublicKeyError> {
    let mut signature_bytes = [0u8; SIGNATURE_LENGTH];
    let bytes_written = STANDARD.decode_slice(data, &mut signature_bytes)?;

    if bytes_written < signature_bytes.len() {
        return Err(PublicKeyError::SignatureTooShort);
    }

    Ok(Signature::from_bytes(&signature_bytes))
}
