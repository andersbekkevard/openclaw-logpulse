use crate::discord::{DiscordConfig, DiscordHttpLookup, DiscordLookup, DiscordLookupError};
use crate::event::NormalizedEvent;
use crate::session_identity::{shorten_non_discord_session_label, SessionRoutingMetadata};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLabelInput {
    pub session_id: String,
    pub raw_label: String,
    pub routing: SessionRoutingMetadata,
}

impl SessionLabelInput {
    pub fn from_event(event: &NormalizedEvent) -> Option<Self> {
        Some(Self {
            session_id: event.durable_session_id()?.to_string(),
            raw_label: event
                .session_label()
                .cloned()
                .unwrap_or_else(|| event.durable_session_id().unwrap_or("<unknown>").to_string()),
            routing: event.routing.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionLabelSource {
    NonDiscord,
    Discord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionLabelState {
    Pending {
        display: String,
        channel_id: String,
    },
    Resolved {
        display: String,
        source: SessionLabelSource,
        channel_id: Option<String>,
    },
    Failed {
        display: String,
        channel_id: String,
        error: DiscordLookupError,
    },
}

impl SessionLabelState {
    pub fn display(&self) -> &str {
        match self {
            SessionLabelState::Pending { display, .. }
            | SessionLabelState::Resolved { display, .. }
            | SessionLabelState::Failed { display, .. } => display.as_str(),
        }
    }
}

struct LookupRequest {
    channel_id: String,
}

struct LookupResult {
    channel_id: String,
    completed_at: DateTime<Utc>,
    outcome: Result<String, DiscordLookupError>,
}

struct LookupWorker {
    requests: Sender<LookupRequest>,
    results: Receiver<LookupResult>,
}

#[derive(Clone, Debug)]
enum CacheState {
    Pending,
    Resolved(String),
    Failed(DiscordLookupError),
}

#[derive(Clone, Debug)]
struct CacheEntry {
    state: CacheState,
    updated_at: DateTime<Utc>,
}

pub struct SessionLabelResolver {
    ttl: Duration,
    sessions: HashMap<String, SessionLabelInput>,
    cache: HashMap<String, CacheEntry>,
    worker: Option<LookupWorker>,
    unavailable_error: Option<DiscordLookupError>,
}

impl SessionLabelResolver {
    pub fn from_env(ttl: Duration) -> Self {
        match DiscordConfig::from_env() {
            Ok(config) => Self::with_lookup(ttl, DiscordHttpLookup::new(config)),
            Err(error) => Self::unavailable(ttl, error),
        }
    }

    pub fn with_lookup<L>(ttl: Duration, lookup: L) -> Self
    where
        L: DiscordLookup,
    {
        let (request_tx, request_rx) = mpsc::channel::<LookupRequest>();
        let (result_tx, result_rx) = mpsc::channel::<LookupResult>();
        thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let outcome = lookup.lookup_channel_name(&request.channel_id);
                if result_tx
                    .send(LookupResult {
                        channel_id: request.channel_id,
                        completed_at: Utc::now(),
                        outcome,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            ttl,
            sessions: HashMap::new(),
            cache: HashMap::new(),
            worker: Some(LookupWorker {
                requests: request_tx,
                results: result_rx,
            }),
            unavailable_error: None,
        }
    }

    pub fn unavailable(ttl: Duration, error: DiscordLookupError) -> Self {
        Self {
            ttl,
            sessions: HashMap::new(),
            cache: HashMap::new(),
            worker: None,
            unavailable_error: Some(error),
        }
    }

    pub fn observe_session(&mut self, input: SessionLabelInput, now: DateTime<Utc>) -> bool {
        let changed = self
            .sessions
            .insert(input.session_id.clone(), input.clone())
            .as_ref()
            != Some(&input);
        self.ensure_lookup(&input, now) || changed
    }

    pub fn refresh(&mut self, now: DateTime<Utc>) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.worker {
            while let Ok(result) = worker.results.try_recv() {
                changed = true;
                self.cache.insert(
                    result.channel_id,
                    CacheEntry {
                        state: match result.outcome {
                            Ok(name) => CacheState::Resolved(name),
                            Err(error) => CacheState::Failed(error),
                        },
                        updated_at: result.completed_at,
                    },
                );
            }
        }

        let inputs = self.sessions.values().cloned().collect::<Vec<_>>();
        for input in inputs {
            changed = self.ensure_lookup(&input, now) || changed;
        }
        changed
    }

    pub fn state_for_session(
        &self,
        session_id: &str,
        fallback_raw_label: Option<&str>,
    ) -> SessionLabelState {
        let fallback = fallback_raw_label.unwrap_or(session_id);
        let Some(input) = self.sessions.get(session_id) else {
            return resolved_non_discord(fallback);
        };
        self.state_for_input(input)
    }

    pub fn state_for_event(&self, event: &NormalizedEvent) -> SessionLabelState {
        if let Some(input) = SessionLabelInput::from_event(event) {
            if let Some(cached) = self.sessions.get(&input.session_id) {
                return self.state_for_input(cached);
            }
            return self.state_for_input(&input);
        }
        resolved_non_discord(
            event.session_label().map(|value| value.as_str()).unwrap_or("-"),
        )
    }

    fn state_for_input(&self, input: &SessionLabelInput) -> SessionLabelState {
        if !input.routing.is_discord() {
            return resolved_non_discord(&input.raw_label);
        }

        let Some(channel_id) = input.routing.channel_id.as_ref() else {
            return resolved_non_discord(&input.raw_label);
        };

        match self.cache.get(channel_id).map(|entry| &entry.state) {
            Some(CacheState::Pending) => SessionLabelState::Pending {
                display: pending_display(channel_id),
                channel_id: channel_id.clone(),
            },
            Some(CacheState::Resolved(channel_name)) => SessionLabelState::Resolved {
                display: format!("#{}", channel_name),
                source: SessionLabelSource::Discord,
                channel_id: Some(channel_id.clone()),
            },
            Some(CacheState::Failed(error)) => SessionLabelState::Failed {
                display: failed_display(channel_id),
                channel_id: channel_id.clone(),
                error: error.clone(),
            },
            None => self
                .unavailable_error
                .clone()
                .map(|error| SessionLabelState::Failed {
                    display: failed_display(channel_id),
                    channel_id: channel_id.clone(),
                    error,
                })
                .unwrap_or_else(|| SessionLabelState::Pending {
                    display: pending_display(channel_id),
                    channel_id: channel_id.clone(),
                }),
        }
    }

    fn ensure_lookup(&mut self, input: &SessionLabelInput, now: DateTime<Utc>) -> bool {
        if !input.routing.is_discord() {
            return false;
        }

        let Some(channel_id) = input.routing.channel_id.as_ref() else {
            return false;
        };

        let should_refresh = match self.cache.get(channel_id) {
            Some(CacheEntry {
                state: CacheState::Pending,
                ..
            }) => false,
            Some(entry) => now.signed_duration_since(entry.updated_at) >= self.ttl,
            None => true,
        };

        if !should_refresh {
            return false;
        }

        let Some(worker) = &self.worker else {
            let error = self.unavailable_error.clone().unwrap_or_else(|| {
                DiscordLookupError::missing_config("discord lookup worker is not configured")
            });
            self.cache.insert(
                channel_id.clone(),
                CacheEntry {
                    state: CacheState::Failed(error),
                    updated_at: now,
                },
            );
            return true;
        };

        if worker
            .requests
            .send(LookupRequest {
                channel_id: channel_id.clone(),
            })
            .is_err()
        {
            self.cache.insert(
                channel_id.clone(),
                CacheEntry {
                    state: CacheState::Failed(DiscordLookupError::missing_config(
                        "discord lookup worker stopped unexpectedly",
                    )),
                    updated_at: now,
                },
            );
            return true;
        }

        self.cache.insert(
            channel_id.clone(),
            CacheEntry {
                state: CacheState::Pending,
                updated_at: now,
            },
        );
        true
    }
}

fn resolved_non_discord(raw_label: &str) -> SessionLabelState {
    SessionLabelState::Resolved {
        display: shorten_non_discord_session_label(raw_label),
        source: SessionLabelSource::NonDiscord,
        channel_id: None,
    }
}

fn pending_display(channel_id: &str) -> String {
    format!("#{channel_id} (resolving)")
}

fn failed_display(channel_id: &str) -> String {
    format!("#{channel_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::{DiscordLookupError, DiscordLookupErrorKind};
    use crate::session_identity::SessionRoutingMetadata;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    struct ScriptedLookup {
        calls: Arc<AtomicUsize>,
        request_tx: Option<Sender<String>>,
        release_rx: Option<mpsc::Receiver<Result<String, DiscordLookupError>>>,
        result: Result<String, DiscordLookupError>,
    }

    impl ScriptedLookup {
        fn immediate(result: Result<String, DiscordLookupError>, calls: Arc<AtomicUsize>) -> Self {
            Self {
                calls,
                request_tx: None,
                release_rx: None,
                result,
            }
        }

        fn blocking(
            calls: Arc<AtomicUsize>,
        ) -> (
            Self,
            mpsc::Receiver<String>,
            Sender<Result<String, DiscordLookupError>>,
        ) {
            let (request_tx, request_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            (
                Self {
                    calls,
                    request_tx: Some(request_tx),
                    release_rx: Some(release_rx),
                    result: Ok("unused".to_string()),
                },
                request_rx,
                release_tx,
            )
        }
    }

    impl DiscordLookup for ScriptedLookup {
        fn lookup_channel_name(&self, channel_id: &str) -> Result<String, DiscordLookupError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(request_tx) = &self.request_tx {
                request_tx
                    .send(channel_id.to_string())
                    .expect("send channel id to test");
            }
            if let Some(release_rx) = &self.release_rx {
                return release_rx.recv().expect("receive scripted result");
            }
            self.result.clone()
        }
    }

    fn input(raw_label: &str, routing: SessionRoutingMetadata) -> SessionLabelInput {
        SessionLabelInput {
            session_id: "session-1".to_string(),
            raw_label: raw_label.to_string(),
            routing,
        }
    }

    fn discord_routing(channel_id: Option<&str>) -> SessionRoutingMetadata {
        SessionRoutingMetadata {
            provider: Some("discord".to_string()),
            provider_source: Some("payload".to_string()),
            channel_id: channel_id.map(str::to_string),
            channel_id_source: channel_id.map(|_| "payload".to_string()),
            issues: Vec::new(),
        }
    }

    fn wait_for<F>(mut condition: F)
    where
        F: FnMut() -> bool,
    {
        for _ in 0..50 {
            if condition() {
                return;
            }
            thread::sleep(StdDuration::from_millis(10));
        }
        panic!("condition was not met in time");
    }

    #[test]
    fn non_discord_labels_are_shortened_deterministically() {
        let mut resolver = SessionLabelResolver::unavailable(
            Duration::minutes(5),
            DiscordLookupError::missing_config("disabled"),
        );
        resolver.observe_session(
            input(
                "agent:main:workspace:session-42",
                SessionRoutingMetadata::default(),
            ),
            Utc::now(),
        );

        assert_eq!(
            resolver.state_for_session("session-1", None),
            SessionLabelState::Resolved {
                display: "workspace:session-42".to_string(),
                source: SessionLabelSource::NonDiscord,
                channel_id: None,
            }
        );
    }

    #[test]
    fn discord_lookup_is_pending_until_worker_completes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (lookup, request_rx, release_tx) = ScriptedLookup::blocking(calls.clone());
        let mut resolver = SessionLabelResolver::with_lookup(Duration::minutes(5), lookup);
        let now = Utc::now();

        assert!(resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            now,
        ));

        assert_eq!(
            resolver.state_for_session("session-1", None),
            SessionLabelState::Pending {
                display: "#1234567890 (resolving)".to_string(),
                channel_id: "1234567890".to_string(),
            }
        );
        assert_eq!(
            request_rx.recv_timeout(StdDuration::from_secs(1)).expect("request"),
            "1234567890"
        );

        release_tx
            .send(Ok("ops-war-room".to_string()))
            .expect("release result");
        wait_for(|| resolver.refresh(Utc::now()));

        assert_eq!(
            resolver.state_for_session("session-1", None),
            SessionLabelState::Resolved {
                display: "#ops-war-room".to_string(),
                source: SessionLabelSource::Discord,
                channel_id: Some("1234567890".to_string()),
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn discord_failures_are_cached_and_use_channel_id_display() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resolver = SessionLabelResolver::with_lookup(
            Duration::minutes(5),
            ScriptedLookup::immediate(
                Err(DiscordLookupError::not_found("missing channel")),
                calls.clone(),
            ),
        );
        let now = Utc::now();
        resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            now,
        );

        wait_for(|| resolver.refresh(Utc::now()));
        assert_eq!(
            resolver.state_for_session("session-1", None),
            SessionLabelState::Failed {
                display: "#1234567890".to_string(),
                channel_id: "1234567890".to_string(),
                error: DiscordLookupError::not_found("missing channel"),
            }
        );

        resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            now + Duration::minutes(1),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_channel_metadata_stays_on_non_discord_path() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resolver = SessionLabelResolver::with_lookup(
            Duration::minutes(5),
            ScriptedLookup::immediate(Ok("general".to_string()), calls.clone()),
        );
        resolver.observe_session(
            input("agent:main:discord", discord_routing(None)),
            Utc::now(),
        );

        assert_eq!(
            resolver.state_for_session("session-1", None),
            SessionLabelState::Resolved {
                display: "main:discord".to_string(),
                source: SessionLabelSource::NonDiscord,
                channel_id: None,
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn missing_token_failures_are_explicit() {
        let mut resolver = SessionLabelResolver::unavailable(
            Duration::minutes(5),
            DiscordLookupError::missing_token("set LOGPULSE_DISCORD_TOKEN"),
        );
        resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            Utc::now(),
        );

        match resolver.state_for_session("session-1", None) {
            SessionLabelState::Failed {
                display,
                channel_id,
                error,
            } => {
                assert_eq!(display, "#1234567890");
                assert_eq!(channel_id, "1234567890");
                assert_eq!(error.kind, DiscordLookupErrorKind::MissingToken);
            }
            other => panic!("expected failed state, got {other:?}"),
        }
    }

    #[test]
    fn missing_lookup_config_failures_are_explicit() {
        let mut resolver = SessionLabelResolver::unavailable(
            Duration::minutes(5),
            DiscordLookupError::missing_config("discord lookup disabled"),
        );
        resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            Utc::now(),
        );

        match resolver.state_for_session("session-1", None) {
            SessionLabelState::Failed { error, .. } => {
                assert_eq!(error.kind, DiscordLookupErrorKind::MissingConfig);
            }
            other => panic!("expected failed state, got {other:?}"),
        }
    }

    #[test]
    fn cache_retries_after_ttl_expires() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut resolver = SessionLabelResolver::with_lookup(
            Duration::seconds(1),
            ScriptedLookup::immediate(Ok("general".to_string()), calls.clone()),
        );
        let now = Utc::now();
        resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            now,
        );
        wait_for(|| resolver.refresh(Utc::now()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        resolver.observe_session(
            input("agent:main:discord:channel:1234567890", discord_routing(Some("1234567890"))),
            now + Duration::milliseconds(500),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        resolver.refresh(now + Duration::seconds(2));
        wait_for(|| {
            resolver.refresh(Utc::now());
            calls.load(Ordering::SeqCst) == 2
        });
    }

    #[test]
    fn resolver_can_derive_input_from_normalized_event() {
        let event = NormalizedEvent {
            kind: crate::event::ToolEventKind::Other,
            timestamp: None,
            timestamp_raw: None,
            source_path: None,
            source_kind: None,
            session_key: Some("agent:main:discord:channel:1234567890".to_string()),
            session_label: Some("agent:main:discord:channel:1234567890".to_string()),
            session_id: Some("session-1".to_string()),
            session_source: Some("path".to_string()),
            session_label_source: Some("payload".to_string()),
            session_identity_conflicts: Vec::new(),
            routing: discord_routing(Some("1234567890")),
            agent_id: None,
            agent_source: None,
            tool_name: None,
            status: None,
            result_summary: None,
            result_preview: None,
            result_raw: None,
            result_metrics: Vec::new(),
            exit_code: None,
            duration_ms: None,
            is_error: None,
            call_id: None,
            call_ids: Vec::new(),
            correlation_ids: Vec::new(),
            message_id: None,
            parent_message_id: None,
            transcript_tool_call_index: None,
            transcript_tool_call_count: None,
            level: crate::event::Severity::Info,
            level_raw: None,
            params: Vec::new(),
            args_preview: Vec::new(),
            args_raw: Some(json!({})),
            args_truncated: false,
            message: None,
            raw_line: "{}".to_string(),
        };

        assert_eq!(
            SessionLabelInput::from_event(&event),
            Some(SessionLabelInput {
                session_id: "session-1".to_string(),
                raw_label: "agent:main:discord:channel:1234567890".to_string(),
                routing: discord_routing(Some("1234567890")),
            })
        );
    }
}
