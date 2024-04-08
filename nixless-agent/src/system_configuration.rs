use derive_builder::Builder;
use serde::{Deserialize, Serialize};

#[derive(Builder, Clone, Deserialize, Serialize)]
pub struct SystemConfiguration {
    pub version_number: u32,
    pub toplevel_path_string: String,
    #[builder(default)]
    pub paths: Vec<String>,
}

impl SystemConfiguration {
    pub fn builder() -> SystemConfigurationBuilder {
        SystemConfigurationBuilder::default()
    }
}
