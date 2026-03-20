use serde_json::Value;
use std::env;

const DEFAULT_DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const TOKEN_ENV_VARS: &[&str] = &[
    "LOGPULSE_DISCORD_TOKEN",
    "DISCORD_TOKEN",
    "DISCORD_BOT_TOKEN",
];
const API_BASE_ENV_VAR: &str = "LOGPULSE_DISCORD_API_BASE";
const FALLBACK_DISCORD_CHANNELS: &[(&str, &str)] = &[
    ("1477636729950179490", "private-channel-13"),
    ("1477629839455555698", "private-channel-11"),
    ("1477629901833244865", "private-channel-10"),
    ("1477629926034112653", "private-channel-09"),
    ("1477629953666191360", "private-channel-12"),
    ("1477629973337739339", "private-channel-08"),
    ("1477629989963698278", "private-channel-07"),
    ("1478102405659754526", "private-channel-14"),
    ("1480977465487917097", "private-channel-06"),
    ("#1481325120319393822", "private-channel-05"),
    ("1483483883935895594", "private-channel-01"),
    ("1484501154141438003", "private-channel-02"),
    ("1484501153969602662", "private-channel-03"),
    ("1484501154321797211", "private-channel-04"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscordLookupErrorKind {
    MissingConfig,
    MissingToken,
    Network,
    Forbidden,
    NotFound,
    InvalidResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscordLookupError {
    pub kind: DiscordLookupErrorKind,
    pub message: String,
}

impl DiscordLookupError {
    pub fn missing_config(message: impl Into<String>) -> Self {
        Self {
            kind: DiscordLookupErrorKind::MissingConfig,
            message: message.into(),
        }
    }

    pub fn missing_token(message: impl Into<String>) -> Self {
        Self {
            kind: DiscordLookupErrorKind::MissingToken,
            message: message.into(),
        }
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self {
            kind: DiscordLookupErrorKind::Network,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            kind: DiscordLookupErrorKind::Forbidden,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: DiscordLookupErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn invalid_response(message: impl Into<String>) -> Self {
        Self {
            kind: DiscordLookupErrorKind::InvalidResponse,
            message: message.into(),
        }
    }
}

pub trait DiscordLookup: Send + 'static {
    fn lookup_channel_name(&self, channel_id: &str) -> Result<String, DiscordLookupError>;
}

pub struct CompositeDiscordLookup {
    http: Option<DiscordHttpLookup>,
    unavailable_error: Option<DiscordLookupError>,
}

impl CompositeDiscordLookup {
    pub fn from_env() -> Self {
        match DiscordConfig::from_env() {
            Ok(config) => Self {
                http: Some(DiscordHttpLookup::new(config)),
                unavailable_error: None,
            },
            Err(error) => Self {
                http: None,
                unavailable_error: Some(error),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscordConfig {
    pub api_base: String,
    pub token: String,
}

impl DiscordConfig {
    pub fn from_env() -> Result<Self, DiscordLookupError> {
        Self::from_env_with(|key| env::var(key).ok())
    }

    fn from_env_with<F>(mut get: F) -> Result<Self, DiscordLookupError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let token = TOKEN_ENV_VARS
            .iter()
            .find_map(|key| get(key).map(|value| value.trim().to_string()))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DiscordLookupError::missing_token(format!(
                    "set one of {} to enable Discord channel resolution",
                    TOKEN_ENV_VARS.join(", ")
                ))
            })?;

        let api_base = get(API_BASE_ENV_VAR)
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_DISCORD_API_BASE.to_string());

        Ok(Self { api_base, token })
    }
}

pub struct DiscordHttpLookup {
    config: DiscordConfig,
}

impl DiscordHttpLookup {
    pub fn new(config: DiscordConfig) -> Self {
        Self { config }
    }
}

impl DiscordLookup for DiscordHttpLookup {
    fn lookup_channel_name(&self, channel_id: &str) -> Result<String, DiscordLookupError> {
        let url = format!("{}/channels/{}", self.config.api_base, channel_id);
        let response = ureq::get(&url)
            .set("Authorization", &format!("Bot {}", self.config.token))
            .set("User-Agent", "logpulse/0.1")
            .call();

        match response {
            Ok(response) => {
                let value: Value = response.into_json().map_err(|err| {
                    DiscordLookupError::invalid_response(format!(
                        "discord channel response was not valid json: {err}"
                    ))
                })?;
                let name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        DiscordLookupError::invalid_response(
                            "discord channel response did not include a usable name",
                        )
                    })?;
                Ok(name.to_string())
            }
            Err(ureq::Error::Status(403, _)) => Err(DiscordLookupError::forbidden(format!(
                "discord denied access to channel {}",
                channel_id
            ))),
            Err(ureq::Error::Status(404, _)) => Err(DiscordLookupError::not_found(format!(
                "discord channel {} was not found",
                channel_id
            ))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                Err(DiscordLookupError::network(format!(
                    "discord request for channel {} failed with status {}: {}",
                    channel_id, status, body
                )))
            }
            Err(ureq::Error::Transport(err)) => Err(DiscordLookupError::network(format!(
                "discord request for channel {} failed: {}",
                channel_id, err
            ))),
        }
    }
}

impl DiscordLookup for CompositeDiscordLookup {
    fn lookup_channel_name(&self, channel_id: &str) -> Result<String, DiscordLookupError> {
        if let Some(http) = &self.http {
            match http.lookup_channel_name(channel_id) {
                Ok(name) => return Ok(name),
                Err(error) => {
                    return fallback_channel_name(channel_id)
                        .map(str::to_string)
                        .ok_or(error);
                }
            }
        }

        fallback_channel_name(channel_id)
            .map(str::to_string)
            .ok_or_else(|| {
                self.unavailable_error.clone().unwrap_or_else(|| {
                    DiscordLookupError::missing_config("discord lookup worker is not configured")
                })
            })
    }
}

fn fallback_channel_name(channel_id: &str) -> Option<&'static str> {
    FALLBACK_DISCORD_CHANNELS
        .iter()
        .find_map(|(candidate, name)| (*candidate == channel_id).then_some(*name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_token() {
        let result = DiscordConfig::from_env_with(|_| None);
        assert_eq!(
            result,
            Err(DiscordLookupError::missing_token(
                "set one of LOGPULSE_DISCORD_TOKEN, DISCORD_TOKEN, DISCORD_BOT_TOKEN to enable Discord channel resolution"
            ))
        );
    }

    #[test]
    fn config_uses_default_api_base() {
        let config = DiscordConfig::from_env_with(|key| match key {
            "LOGPULSE_DISCORD_TOKEN" => Some("secret-token".to_string()),
            _ => None,
        })
        .expect("config");

        assert_eq!(config.token, "secret-token");
        assert_eq!(config.api_base, DEFAULT_DISCORD_API_BASE);
    }

    #[test]
    fn config_accepts_custom_api_base() {
        let config = DiscordConfig::from_env_with(|key| match key {
            "DISCORD_TOKEN" => Some("secret-token".to_string()),
            "LOGPULSE_DISCORD_API_BASE" => {
                Some("https://discord.example.test/api/v10/".to_string())
            }
            _ => None,
        })
        .expect("config");

        assert_eq!(config.token, "secret-token");
        assert_eq!(config.api_base, "https://discord.example.test/api/v10");
    }

    #[test]
    fn config_falls_back_to_discord_bot_token() {
        let config = DiscordConfig::from_env_with(|key| match key {
            "DISCORD_BOT_TOKEN" => Some("bot-secret".to_string()),
            _ => None,
        })
        .expect("config");

        assert_eq!(config.token, "bot-secret");
        assert_eq!(config.api_base, DEFAULT_DISCORD_API_BASE);
    }

    #[test]
    fn config_prefers_explicit_token_envs_over_discord_bot_token() {
        let config = DiscordConfig::from_env_with(|key| match key {
            "LOGPULSE_DISCORD_TOKEN" => Some("preferred-logpulse-token".to_string()),
            "DISCORD_TOKEN" => Some("preferred-discord-token".to_string()),
            "DISCORD_BOT_TOKEN" => Some("fallback-bot-token".to_string()),
            _ => None,
        })
        .expect("config");

        assert_eq!(config.token, "preferred-logpulse-token");

        let config = DiscordConfig::from_env_with(|key| match key {
            "DISCORD_TOKEN" => Some("preferred-discord-token".to_string()),
            "DISCORD_BOT_TOKEN" => Some("fallback-bot-token".to_string()),
            _ => None,
        })
        .expect("config");

        assert_eq!(config.token, "preferred-discord-token");
    }

    #[test]
    fn composite_lookup_uses_known_channel_fallback_without_token() {
        let lookup = CompositeDiscordLookup {
            http: None,
            unavailable_error: Some(DiscordLookupError::missing_token("missing token")),
        };

        assert_eq!(
            lookup.lookup_channel_name("1477636729950179490"),
            Ok("main".to_string())
        );
        assert_eq!(
            lookup.lookup_channel_name("1483483883935895594"),
            Ok("private-channel-01".to_string())
        );
        assert_eq!(
            lookup.lookup_channel_name("1484501154141438003"),
            Ok("private-channel-02".to_string())
        );
        assert_eq!(
            lookup.lookup_channel_name("1484501153969602662"),
            Ok("private-channel-03".to_string())
        );
        assert_eq!(
            lookup.lookup_channel_name("1484501154321797211"),
            Ok("private-channel-04".to_string())
        );
    }

    #[test]
    fn composite_lookup_preserves_missing_token_for_unknown_channels() {
        let lookup = CompositeDiscordLookup {
            http: None,
            unavailable_error: Some(DiscordLookupError::missing_token("missing token")),
        };

        assert_eq!(
            lookup.lookup_channel_name("1234567890"),
            Err(DiscordLookupError::missing_token("missing token"))
        );
    }
}
