use std::iter::repeat_with;

use anyhow::anyhow;
use narinfo::NarInfo;

use crate::signing::CachePublicKeychain;

pub trait Fingerprint {
    fn fingerprint(&self) -> anyhow::Result<String>;
    fn verify_fingerprint(&self, keychain: &CachePublicKeychain) -> anyhow::Result<bool>;
}

impl Fingerprint for NarInfo<'_> {
    fn fingerprint(&self) -> anyhow::Result<String> {
        let store_path = self
            .store_path
            .rsplit_once("/")
            .ok_or_else(|| {
                anyhow!("this NAR info doesn't have a store path in the expected format")
            })?
            .0;

        let comma_separated_references: String = self
            .references
            .iter()
            .map(|r| format!("{}/{}", store_path, r))
            // TODO: replace the `.zip().flat_map()` with `intersperse_with` once it's stabilised.
            .zip(repeat_with(|| ",".to_string()))
            .flat_map(|(a, b)| [a, b])
            .collect();

        let fingerprint = format!(
            "1;{store_path};{nar_hash};{nar_size};{references}",
            store_path = self.store_path,
            nar_hash = self.nar_hash,
            nar_size = self.nar_size,
            references = comma_separated_references
        );

        Ok(fingerprint)
    }

    fn verify_fingerprint(&self, keychain: &CachePublicKeychain) -> anyhow::Result<bool> {
        let fingerprint = self.fingerprint()?;
        let fingerprint_bytes = fingerprint.as_bytes();

        Ok(self.sigs.iter().any(|sig| {
            keychain
                .verify(&sig.key_name, &fingerprint_bytes, sig.sig.as_bytes())
                .is_ok()
        }))
    }
}
