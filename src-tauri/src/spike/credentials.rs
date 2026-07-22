use std::env;

use crate::spike::error::SpikeError;

pub const DEFAULT_RESOURCE_ID: &str = "volc.seedasr.sauc.duration";

#[derive(Clone)]
pub struct Credentials {
    pub app_id: String,
    pub access_token: String,
    pub resource_id: String,
}

impl Credentials {
    pub fn load() -> Result<Self, SpikeError> {
        let _ = dotenvy::from_filename(".env.local");
        let _ = dotenvy::from_filename("../.env.local");

        let app_id = required("VOICEINPUT_VOLC_APP_ID")?;
        let access_token = required("VOICEINPUT_VOLC_ACCESS_TOKEN")?;
        let resource_id = env::var("VOICEINPUT_VOLC_RESOURCE_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_RESOURCE_ID.to_owned());

        if has_newline(&app_id) || has_newline(&access_token) || has_newline(&resource_id) {
            return Err(SpikeError::CredentialsMissing);
        }

        Ok(Self {
            app_id,
            access_token,
            resource_id,
        })
    }
}

fn required(name: &str) -> Result<String, SpikeError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(SpikeError::CredentialsMissing)
}

fn has_newline(value: &str) -> bool {
    value.contains('\n') || value.contains('\r')
}
