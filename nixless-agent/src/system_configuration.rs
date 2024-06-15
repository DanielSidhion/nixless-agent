use std::collections::HashSet;

use derive_builder::Builder;
use serde::{Deserialize, Serialize};

#[derive(Builder, Clone, Deserialize, Serialize)]
pub struct SystemConfiguration {
    pub version_number: u32,
    pub system_package_id: String,
    #[builder(default)]
    pub package_ids: HashSet<String>,
}

impl SystemConfiguration {
    pub fn builder() -> SystemConfigurationBuilder {
        SystemConfigurationBuilder::default()
    }

    pub fn tombstone() -> Self {
        Self {
            version_number: 0,
            system_package_id: "unknown".to_string(),
            package_ids: HashSet::new(),
        }
    }

    pub fn is_tombstone(&self) -> bool {
        self.version_number == 0
            && self.system_package_id == "unknown"
            && self.package_ids.is_empty()
    }
}
