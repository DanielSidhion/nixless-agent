use derive_builder::Builder;
use serde::{Deserialize, Serialize};

#[derive(Builder, Clone, Deserialize, Serialize)]
pub struct SystemConfiguration {
    pub version_number: u32,
    pub system_package_id: String,
    #[builder(default)]
    pub package_ids: Vec<String>,
}

impl SystemConfiguration {
    pub fn builder() -> SystemConfigurationBuilder {
        SystemConfigurationBuilder::default()
    }
}
