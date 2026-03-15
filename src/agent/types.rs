// TODO: Remove this lint
// Currently not documented because interface of this module is still changing
#![allow(missing_docs)]

use bytes::Bytes;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

// ---------------------------------------------------------------------------
// Settings (client sends as first message)
// ---------------------------------------------------------------------------

/// Settings for configuring the Agent WebSocket session.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSettings {
    /// Audio input/output configuration.
    pub audio: AudioSettings,
    /// Agent behavior configuration.
    pub agent: AgentConfig,
    /// Optional tags for tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Opt out of model improvement program.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mip_opt_out: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioSettings {
    pub input: AudioInputSettings,
    pub output: AudioOutputSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioInputSettings {
    pub encoding: String,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioOutputSettings {
    pub encoding: String,
    pub sample_rate: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub listen: ListenConfig,
    pub think: ThinkConfig,
    pub speak: SpeakConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListenConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ListenProvider>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListenProvider {
    #[serde(rename = "type")]
    pub provider_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyterms: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThinkConfig {
    pub provider: ThinkProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functions: Option<Vec<FunctionDefinition>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThinkProvider {
    #[serde(rename = "type")]
    pub provider_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeakConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<SpeakProvider>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeakProvider {
    #[serde(rename = "type")]
    pub provider_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_side: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentContext {
    pub messages: Vec<ContextMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextMessage {
    pub role: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// Server events
// ---------------------------------------------------------------------------

/// Events received from the Agent WebSocket server.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    /// Binary audio data from the agent.
    Audio(Bytes),
    /// Welcome message with session ID.
    Welcome { request_id: String },
    /// Settings were applied successfully.
    SettingsApplied,
    /// Conversation text (user or agent speech transcription).
    ConversationText { role: String, content: String },
    /// User started speaking.
    UserStartedSpeaking,
    /// Agent is thinking/processing.
    AgentThinking { content: String },
    /// Agent started producing audio.
    AgentStartedSpeaking {
        total_latency: Option<f64>,
        tts_latency: Option<f64>,
        ttt_latency: Option<f64>,
    },
    /// Agent finished sending audio.
    AgentAudioDone,
    /// Function call request from agent.
    FunctionCallRequest { functions: Vec<FunctionCall> },
    /// Function call response received.
    FunctionCallResponse {
        id: String,
        name: String,
        content: String,
    },
    /// Prompt was updated.
    PromptUpdated,
    /// Speak settings were updated.
    SpeakUpdated,
    /// Think settings were updated.
    ThinkUpdated,
    /// Injection was refused.
    InjectionRefused { message: String },
    /// Non-fatal warning.
    Warning { code: String, description: String },
    /// Error event.
    Error { code: String, description: String },
    /// Unknown event type (forward-compatibility).
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub client_side: bool,
}

// -- Two-stage deserialization (following flux_response.rs pattern) ----------

#[derive(Deserialize)]
#[serde(tag = "type")]
enum TaggedAgentEvent {
    Welcome {
        request_id: String,
    },
    SettingsApplied {},
    ConversationText {
        role: String,
        content: String,
    },
    UserStartedSpeaking {},
    AgentThinking {
        content: String,
    },
    AgentStartedSpeaking {
        total_latency: Option<f64>,
        tts_latency: Option<f64>,
        ttt_latency: Option<f64>,
    },
    AgentAudioDone {},
    FunctionCallRequest {
        functions: Vec<FunctionCall>,
    },
    FunctionCallResponse {
        id: String,
        name: String,
        content: String,
    },
    PromptUpdated {},
    SpeakUpdated {},
    ThinkUpdated {},
    InjectionRefused {
        message: String,
    },
    Warning {
        code: String,
        description: String,
    },
    Error {
        code: String,
        description: String,
    },
}

impl From<TaggedAgentEvent> for AgentEvent {
    fn from(tagged: TaggedAgentEvent) -> Self {
        match tagged {
            TaggedAgentEvent::Welcome { request_id } => AgentEvent::Welcome { request_id },
            TaggedAgentEvent::SettingsApplied {} => AgentEvent::SettingsApplied,
            TaggedAgentEvent::ConversationText { role, content } => {
                AgentEvent::ConversationText { role, content }
            }
            TaggedAgentEvent::UserStartedSpeaking {} => AgentEvent::UserStartedSpeaking,
            TaggedAgentEvent::AgentThinking { content } => AgentEvent::AgentThinking { content },
            TaggedAgentEvent::AgentStartedSpeaking {
                total_latency,
                tts_latency,
                ttt_latency,
            } => AgentEvent::AgentStartedSpeaking {
                total_latency,
                tts_latency,
                ttt_latency,
            },
            TaggedAgentEvent::AgentAudioDone {} => AgentEvent::AgentAudioDone,
            TaggedAgentEvent::FunctionCallRequest { functions } => {
                AgentEvent::FunctionCallRequest { functions }
            }
            TaggedAgentEvent::FunctionCallResponse { id, name, content } => {
                AgentEvent::FunctionCallResponse { id, name, content }
            }
            TaggedAgentEvent::PromptUpdated {} => AgentEvent::PromptUpdated,
            TaggedAgentEvent::SpeakUpdated {} => AgentEvent::SpeakUpdated,
            TaggedAgentEvent::ThinkUpdated {} => AgentEvent::ThinkUpdated,
            TaggedAgentEvent::InjectionRefused { message } => {
                AgentEvent::InjectionRefused { message }
            }
            TaggedAgentEvent::Warning { code, description } => {
                AgentEvent::Warning { code, description }
            }
            TaggedAgentEvent::Error { code, description } => {
                AgentEvent::Error { code, description }
            }
        }
    }
}

/// All known `type` values for server events.
const KNOWN_AGENT_EVENT_TYPES: &[&str] = &[
    "Welcome",
    "SettingsApplied",
    "ConversationText",
    "UserStartedSpeaking",
    "AgentThinking",
    "AgentStartedSpeaking",
    "AgentAudioDone",
    "FunctionCallRequest",
    "FunctionCallResponse",
    "PromptUpdated",
    "SpeakUpdated",
    "ThinkUpdated",
    "InjectionRefused",
    "Warning",
    "Error",
];

impl<'de> Deserialize<'de> for AgentEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        let type_str = value.get("type").and_then(|t| t.as_str());

        match type_str {
            Some(t) if KNOWN_AGENT_EVENT_TYPES.contains(&t) => {
                serde_json::from_value::<TaggedAgentEvent>(value)
                    .map(AgentEvent::from)
                    .map_err(de::Error::custom)
            }
            _ => Ok(AgentEvent::Unknown(value)),
        }
    }
}

// ---------------------------------------------------------------------------
// Client messages (sent to server)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub(crate) enum AgentClientMessage {
    Settings(SettingsPayload),
    UpdatePrompt {
        instructions: String,
    },
    UpdateSpeak {
        speak: SpeakConfig,
    },
    UpdateThink {
        think: ThinkConfig,
    },
    InjectUserMessage {
        content: String,
    },
    InjectAgentMessage {
        message: String,
    },
    FunctionCallResponse {
        function_call_id: String,
        output: String,
    },
    KeepAlive {},
}

/// Wrapper that flattens `AgentSettings` fields into the `Settings` variant.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SettingsPayload {
    #[serde(flatten)]
    pub settings: AgentSettings,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_welcome() {
        let json = r#"{"type": "Welcome", "request_id": "abc-123"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::Welcome { request_id } => assert_eq!(request_id, "abc-123"),
            _ => panic!("expected Welcome"),
        }
    }

    #[test]
    fn deserialize_settings_applied() {
        let json = r#"{"type": "SettingsApplied"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, AgentEvent::SettingsApplied));
    }

    #[test]
    fn deserialize_conversation_text() {
        let json = r#"{"type": "ConversationText", "role": "user", "content": "hello"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::ConversationText { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content, "hello");
            }
            _ => panic!("expected ConversationText"),
        }
    }

    #[test]
    fn deserialize_function_call_request() {
        let json = r#"{
            "type": "FunctionCallRequest",
            "functions": [
                {"id": "fn-1", "name": "get_weather", "arguments": "{\"city\":\"NYC\"}", "client_side": true}
            ]
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::FunctionCallRequest { functions } => {
                assert_eq!(functions.len(), 1);
                assert_eq!(functions[0].name, "get_weather");
                assert!(functions[0].client_side);
            }
            _ => panic!("expected FunctionCallRequest"),
        }
    }

    #[test]
    fn deserialize_error() {
        let json = r#"{"type": "Error", "code": "ERR_001", "description": "bad request"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::Error { code, description } => {
                assert_eq!(code, "ERR_001");
                assert_eq!(description, "bad request");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn deserialize_warning() {
        let json = r#"{"type": "Warning", "code": "WARN_01", "description": "heads up"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::Warning { code, description } => {
                assert_eq!(code, "WARN_01");
                assert_eq!(description, "heads up");
            }
            _ => panic!("expected Warning"),
        }
    }

    #[test]
    fn deserialize_unknown_type() {
        let json = r#"{"type": "FutureFeature", "data": 42}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::Unknown(value) => {
                assert_eq!(value["type"], "FutureFeature");
                assert_eq!(value["data"], 42);
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn deserialize_missing_type_field() {
        let json = r#"{"some_random": "message"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, AgentEvent::Unknown(_)));
    }

    #[test]
    fn serialize_settings_message() {
        let settings = AgentSettings {
            audio: AudioSettings {
                input: AudioInputSettings {
                    encoding: "linear16".into(),
                    sample_rate: 16000,
                },
                output: AudioOutputSettings {
                    encoding: "linear16".into(),
                    sample_rate: 16000,
                    bitrate: None,
                    container: None,
                },
            },
            agent: AgentConfig {
                language: None,
                listen: ListenConfig { provider: None },
                think: ThinkConfig {
                    provider: ThinkProvider {
                        provider_type: "open_ai".into(),
                        model: Some("gpt-4o-mini".into()),
                        instructions: Some("You are a helpful assistant.".into()),
                    },
                    functions: None,
                },
                speak: SpeakConfig { provider: None },
                greeting: Some("Hello!".into()),
                context: None,
            },
            tags: None,
            mip_opt_out: None,
        };

        let msg = AgentClientMessage::Settings(SettingsPayload { settings });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "Settings");
        assert_eq!(json["audio"]["input"]["encoding"], "linear16");
        assert_eq!(json["agent"]["think"]["provider"]["type"], "open_ai");
        assert_eq!(json["agent"]["greeting"], "Hello!");
    }

    #[test]
    fn serialize_update_prompt() {
        let msg = AgentClientMessage::UpdatePrompt {
            instructions: "Be concise.".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "UpdatePrompt");
        assert_eq!(json["instructions"], "Be concise.");
    }

    #[test]
    fn serialize_function_call_response() {
        let msg = AgentClientMessage::FunctionCallResponse {
            function_call_id: "fn-1".into(),
            output: r#"{"temp": 72}"#.into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "FunctionCallResponse");
        assert_eq!(json["function_call_id"], "fn-1");
    }

    #[test]
    fn serialize_keep_alive() {
        let msg = AgentClientMessage::KeepAlive {};
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"KeepAlive"}"#);
    }

    #[test]
    fn serialize_settings_full() {
        let settings = AgentSettings {
            audio: AudioSettings {
                input: AudioInputSettings {
                    encoding: "linear16".into(),
                    sample_rate: 16000,
                },
                output: AudioOutputSettings {
                    encoding: "linear16".into(),
                    sample_rate: 24000,
                    bitrate: Some(128000),
                    container: Some("ogg".into()),
                },
            },
            agent: AgentConfig {
                language: Some("en".into()),
                listen: ListenConfig {
                    provider: Some(ListenProvider {
                        provider_type: "deepgram".into(),
                        model: Some("nova-2".into()),
                        language: Some("en-US".into()),
                        keyterms: Some(vec!["deepgram".into()]),
                    }),
                },
                think: ThinkConfig {
                    provider: ThinkProvider {
                        provider_type: "open_ai".into(),
                        model: Some("gpt-4o".into()),
                        instructions: Some("Be helpful.".into()),
                    },
                    functions: Some(vec![FunctionDefinition {
                        name: "get_weather".into(),
                        description: "Get the weather".into(),
                        parameters: serde_json::json!({"type": "object"}),
                        client_side: Some(true),
                    }]),
                },
                speak: SpeakConfig {
                    provider: Some(SpeakProvider {
                        provider_type: "deepgram".into(),
                        model: Some("aura-asteria-en".into()),
                    }),
                },
                greeting: Some("Hello!".into()),
                context: Some(AgentContext {
                    messages: vec![ContextMessage {
                        role: "assistant".into(),
                        content: "Welcome back!".into(),
                    }],
                }),
            },
            tags: Some(vec!["test".into()]),
            mip_opt_out: Some(true),
        };

        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["audio"]["output"]["bitrate"], 128000);
        assert_eq!(json["agent"]["language"], "en");
        assert_eq!(json["tags"][0], "test");
        assert_eq!(json["mip_opt_out"], true);
    }

    #[test]
    fn deserialize_agent_started_speaking() {
        let json = r#"{"type": "AgentStartedSpeaking", "total_latency": 0.5, "tts_latency": 0.2, "ttt_latency": 0.3}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::AgentStartedSpeaking {
                total_latency,
                tts_latency,
                ttt_latency,
            } => {
                assert_eq!(total_latency, Some(0.5));
                assert_eq!(tts_latency, Some(0.2));
                assert_eq!(ttt_latency, Some(0.3));
            }
            _ => panic!("expected AgentStartedSpeaking"),
        }
    }

    #[test]
    fn deserialize_agent_audio_done() {
        let json = r#"{"type": "AgentAudioDone"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, AgentEvent::AgentAudioDone));
    }

    #[test]
    fn deserialize_user_started_speaking() {
        let json = r#"{"type": "UserStartedSpeaking"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, AgentEvent::UserStartedSpeaking));
    }

    #[test]
    fn deserialize_injection_refused() {
        let json = r#"{"type": "InjectionRefused", "message": "not allowed"}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        match event {
            AgentEvent::InjectionRefused { message } => assert_eq!(message, "not allowed"),
            _ => panic!("expected InjectionRefused"),
        }
    }
}
