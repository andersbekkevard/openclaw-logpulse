use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const DEFAULT_DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const TOKEN_ENV_VARS: &[&str] = &[
    "LOGPULSE_DISCORD_TOKEN",
    "DISCORD_TOKEN",
    "DISCORD_BOT_TOKEN",
];
const API_BASE_ENV_VAR: &str = "LOGPULSE_DISCORD_API_BASE";
const CHANNEL_MAP_FILE_ENV_VAR: &str = "LOGPULSE_DISCORD_CHANNEL_MAP_FILE";
const DEFAULT_CHANNEL_MAP_RELATIVE_PATH: &str = ".openclaw/logpulse/discord_channels.json";

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
    fallback_channels: DiscordChannelFallbacks,
    unavailable_error: Option<DiscordLookupError>,
}

impl CompositeDiscordLookup {
    pub fn from_env() -> Self {
        Self::from_env_with(|key| env::var(key).ok())
    }

    fn from_env_with<F>(mut get: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let (http, unavailable_error) = match DiscordConfig::from_env_with(&mut get) {
            Ok(config) => (Some(DiscordHttpLookup::new(config)), None),
            Err(error) => (None, Some(error)),
        };
        let fallback_channels = DiscordChannelFallbacks::from_env_with(get);

        Self {
            http,
            fallback_channels,
            unavailable_error,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscordConfig {
    pub api_base: String,
    pub token: String,
}

impl DiscordConfig {
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
                    if let Some(name) = self.fallback_channels.lookup(channel_id) {
                        return Ok(name.to_string());
                    }

                    return Err(self.fallback_channels.load_error().unwrap_or(error));
                }
            }
        }

        if let Some(name) = self.fallback_channels.lookup(channel_id) {
            return Ok(name.to_string());
        }

        Err(self.fallback_channels.load_error().unwrap_or_else(|| {
            self.unavailable_error.clone().unwrap_or_else(|| {
                DiscordLookupError::missing_config("discord lookup worker is not configured")
            })
        }))
    }
}

#[derive(Clone, Debug, Default)]
struct DiscordChannelFallbacks {
    names: HashMap<String, String>,
    load_error: Option<DiscordLookupError>,
}

impl DiscordChannelFallbacks {
    fn from_env_with<F>(mut get: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let path = channel_map_path_from_env(&mut get);
        Self::from_path(path.as_deref())
    }

    fn from_path(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };

        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                return Self {
                    names: HashMap::new(),
                    load_error: Some(DiscordLookupError::missing_config(format!(
                        "failed to read discord channel map file {}: {}",
                        path.display(),
                        error
                    ))),
                };
            }
        };

        let parsed = match serde_json::from_str::<Value>(&contents) {
            Ok(Value::Object(entries)) => entries,
            Ok(_) => {
                return Self {
                    names: HashMap::new(),
                    load_error: Some(DiscordLookupError::invalid_response(format!(
                        "discord channel map file {} must contain a JSON object",
                        path.display()
                    ))),
                };
            }
            Err(error) => {
                return Self {
                    names: HashMap::new(),
                    load_error: Some(DiscordLookupError::invalid_response(format!(
                        "failed to parse discord channel map file {}: {}",
                        path.display(),
                        error
                    ))),
                };
            }
        };

        let mut names = HashMap::new();
        for (channel_id, value) in parsed {
            let Some(normalized_id) = normalize_channel_map_key(&channel_id) else {
                return Self {
                    names: HashMap::new(),
                    load_error: Some(DiscordLookupError::invalid_response(format!(
                        "discord channel map file {} contains an empty channel id key",
                        path.display()
                    ))),
                };
            };

            let Some(name) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return Self {
                    names: HashMap::new(),
                    load_error: Some(DiscordLookupError::invalid_response(format!(
                        "discord channel map file {} contains a non-string or empty name for channel {}",
                        path.display(),
                        channel_id
                    ))),
                };
            };

            names.insert(normalized_id.to_string(), name.to_string());
        }

        Self {
            names,
            load_error: None,
        }
    }

    fn lookup(&self, channel_id: &str) -> Option<&str> {
        let normalized = normalize_channel_map_key(channel_id)?;
        self.names.get(normalized).map(String::as_str)
    }

    fn load_error(&self) -> Option<DiscordLookupError> {
        self.load_error.clone()
    }
}

fn channel_map_path_from_env<F>(mut get: F) -> Option<PathBuf>
where
    F: FnMut(&str) -> Option<String>,
{
    let home = get("HOME")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    get(CHANNEL_MAP_FILE_ENV_VAR)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| expand_home_prefix(&value, home.as_deref()))
        .or_else(|| home.map(|value| PathBuf::from(value).join(DEFAULT_CHANNEL_MAP_RELATIVE_PATH)))
}

fn expand_home_prefix(path: &str, home: Option<&str>) -> PathBuf {
    if path == "~" {
        return home
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }

    if let Some(suffix) = path.strip_prefix("~/") {
        return home
            .map(|value| PathBuf::from(value).join(suffix))
            .unwrap_or_else(|| PathBuf::from(path));
    }

    PathBuf::from(path)
}

fn normalize_channel_map_key(channel_id: &str) -> Option<&str> {
    let trimmed = channel_id.trim();
    let normalized = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::{fs, path::Path};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = format!(
                "logpulse-{prefix}-{}-{}",
                process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time")
                    .as_nanos()
            );
            let path = env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

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
    fn missing_channel_map_file_is_non_fatal() {
        let temp_dir = TempDir::new("missing-channel-map");
        let map_path = temp_dir.path().join("missing.json");
        let fallback = DiscordChannelFallbacks::from_env_with(|key| match key {
            CHANNEL_MAP_FILE_ENV_VAR => Some(map_path.display().to_string()),
            _ => None,
        });

        let lookup = CompositeDiscordLookup {
            http: None,
            fallback_channels: fallback,
            unavailable_error: Some(DiscordLookupError::missing_token("missing token")),
        };

        assert_eq!(
            lookup.lookup_channel_name("1234567890"),
            Err(DiscordLookupError::missing_token("missing token"))
        );
    }

    #[test]
    fn malformed_channel_map_surfaces_invalid_response() {
        let temp_dir = TempDir::new("malformed-channel-map");
        let map_path = temp_dir.path().join("discord_channels.json");
        fs::write(&map_path, "{").expect("write malformed map");

        let lookup = CompositeDiscordLookup {
            http: None,
            fallback_channels: DiscordChannelFallbacks::from_env_with(|key| match key {
                CHANNEL_MAP_FILE_ENV_VAR => Some(map_path.display().to_string()),
                _ => None,
            }),
            unavailable_error: Some(DiscordLookupError::missing_token("missing token")),
        };

        let error = lookup
            .lookup_channel_name("1234567890")
            .expect_err("malformed map should fail");
        assert_eq!(error.kind, DiscordLookupErrorKind::InvalidResponse);
        assert!(error
            .message
            .contains("failed to parse discord channel map file"));
        assert!(error.message.contains(&map_path.display().to_string()));
    }

    #[test]
    fn composite_lookup_uses_loaded_channel_fallback_without_token() {
        let temp_dir = TempDir::new("loaded-channel-map");
        let map_path = temp_dir.path().join("discord_channels.json");
        fs::write(
            &map_path,
            r#"{
  "111111111111111111": "alpha-room",
  "222222222222222222": "beta-room"
}"#,
        )
        .expect("write channel map");

        let lookup = CompositeDiscordLookup {
            http: None,
            fallback_channels: DiscordChannelFallbacks::from_env_with(|key| match key {
                CHANNEL_MAP_FILE_ENV_VAR => Some(map_path.display().to_string()),
                _ => None,
            }),
            unavailable_error: Some(DiscordLookupError::missing_token("missing token")),
        };

        assert_eq!(
            lookup.lookup_channel_name("111111111111111111"),
            Ok("alpha-room".to_string())
        );
        assert_eq!(
            lookup.lookup_channel_name("#222222222222222222"),
            Ok("beta-room".to_string())
        );
    }
}
