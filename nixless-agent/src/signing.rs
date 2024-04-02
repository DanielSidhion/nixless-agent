use std::collections::HashMap;

use anyhow::anyhow;
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH};

pub struct CachePublicKey {
    name: String,
    key: VerifyingKey,
}

impl CachePublicKey {
    /// Nix stores keys in the format `<name>:<base64str>`, where `<name>` is the name of the key as used by the cache, and `<base64str>` is a base64-encoded string of the bytes of the key.
    pub fn from_nix_format(s: &str) -> anyhow::Result<Self> {
        if let [name, base64str] = s.split(":").collect::<Vec<_>>()[..] {
            let mut key_bytes = [0u8; PUBLIC_KEY_LENGTH];
            let bytes_written = STANDARD.decode_slice(base64str, &mut key_bytes)?;

            if bytes_written < key_bytes.len() {
                return Err(anyhow!("The string with the public key isn't long enough!"));
            }

            Ok(Self {
                name: name.to_string(),
                key: VerifyingKey::from_bytes(&key_bytes)?,
            })
        } else {
            Err(anyhow!("Received string in unexpected format"))
        }
    }
}

pub struct CachePublicKeychain {
    keys: HashMap<String, CachePublicKey>,
}

impl CachePublicKeychain {
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
        }
    }

    pub fn with_known_keys() -> anyhow::Result<Self> {
        let mut this = Self::new();
        let nixos_key = CachePublicKey::from_nix_format(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=",
        )?;
        this.add_key(nixos_key)?;

        Ok(this)
    }

    pub fn add_key(&mut self, key: CachePublicKey) -> anyhow::Result<()> {
        if self.keys.contains_key(&key.name) {
            Err(anyhow!("key already exists!"))
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
    ) -> anyhow::Result<bool> {
        if let Some(key) = self.keys.get(key_name) {
            let mut signature_bytes = [0u8; SIGNATURE_LENGTH];
            let bytes_written = STANDARD.decode_slice(signature_base64, &mut signature_bytes)?;

            if bytes_written < signature_bytes.len() {
                return Err(anyhow!("The given signature isn't long enough!"));
            }

            let signature = Signature::from_bytes(&signature_bytes);
            Ok(key.key.verify(data, &signature).is_ok())
        } else {
            Ok(false)
        }
    }
}
