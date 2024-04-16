use narinfo::{NarInfo, Sig};

pub struct OwnedSig {
    pub key_name: String,
    pub sig: String,
}

impl<'a> From<Sig<'a>> for OwnedSig {
    fn from(value: Sig<'a>) -> Self {
        Self {
            key_name: value.key_name.to_string(),
            sig: value.sig.to_string(),
        }
    }
}

pub struct OwnedNarInfo {
    pub store_path: String,
    pub url: String,
    pub compression: Option<String>,
    pub nar_hash: String,
    pub nar_size: usize,
    pub file_hash: Option<String>,
    pub file_size: Option<usize>,
    pub deriver: Option<String>,
    pub system: Option<String>,
    pub references: Vec<String>,
    pub sigs: Vec<OwnedSig>,
}

impl<'a> From<NarInfo<'a>> for OwnedNarInfo {
    fn from(value: NarInfo<'a>) -> Self {
        Self {
            store_path: value.store_path.to_string(),
            url: value.url.to_string(),
            compression: value.compression.map(|v| v.to_string()),
            nar_hash: value.nar_hash.to_string(),
            nar_size: value.nar_size,
            file_hash: value.file_hash.map(|v| v.to_string()),
            file_size: value.file_size.clone(),
            deriver: value.deriver.map(|v| v.to_string()),
            system: value.system.map(|v| v.to_string()),
            references: value
                .references
                .into_iter()
                .map(|v| v.to_string())
                .collect(),
            sigs: value.sigs.into_iter().map(|v| v.into()).collect(),
        }
    }
}
