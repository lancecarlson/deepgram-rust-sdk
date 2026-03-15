// TODO: Remove this lint
// Currently not documented because interface of this module is still changing
#![allow(missing_docs)]

//! WebSocket client for the Deepgram Agent API.
//!
//! See the [Deepgram Agent API Reference][api] for more info.
//!
//! [api]: https://developers.deepgram.com/docs/voicebot

use std::{
    error::Error,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    select_biased,
    stream::StreamExt,
    SinkExt, Stream,
};
use http::Request;
use pin_project::pin_project;
use tokio_tungstenite::{tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};
use tungstenite::{
    handshake::client,
    protocol::frame::coding::{Data, OpCode},
    Utf8Bytes,
};
use url::Url;

use super::types::{
    AgentClientMessage, AgentEvent, AgentSettings, SettingsPayload, SpeakConfig, ThinkConfig,
};
use crate::{Agent, Deepgram, DeepgramError, Result};

static AGENT_URL_PATH: &str = "v1/agent/converse";

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AgentBuilder<'a> {
    deepgram: &'a Deepgram,
    settings: AgentSettings,
}

impl Agent<'_> {
    /// Begin configuring an Agent WebSocket connection.
    pub fn converse(&self, settings: AgentSettings) -> AgentBuilder<'_> {
        AgentBuilder {
            deepgram: self.0,
            settings,
        }
    }
}

impl AgentBuilder<'_> {
    fn build_url(&self) -> Result<Url> {
        let mut url = self.deepgram.base_url.join(AGENT_URL_PATH).expect(
            "base_url is checked to be a valid base_url when constructing Deepgram client",
        );

        match url.scheme() {
            "http" | "ws" => url
                .set_scheme("ws")
                .expect("a valid conversion according to the .set_scheme docs"),
            "https" | "wss" => url
                .set_scheme("wss")
                .expect("a valid conversion according to the .set_scheme docs"),
            _ => unreachable!(
                "base_url is validated to have a scheme of http, https, ws, or wss when constructing Deepgram client"
            ),
        }
        Ok(url)
    }

    /// Create a low-level [`AgentHandle`] for full control over the connection.
    pub async fn handle(self) -> Result<AgentHandle> {
        AgentHandle::new(self).await
    }

    /// Create a high-level [`AgentStream`] that feeds audio from the given
    /// stream and yields server events.
    pub async fn stream<S, E>(self, audio_stream: S) -> Result<AgentStream>
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin + 'static,
        E: Error + Send + Sync + 'static,
    {
        let mut handle = self.handle().await?;

        let (mut tx, rx) = mpsc::channel(1);

        tokio::task::spawn(async move {
            let mut audio_stream = audio_stream.fuse();

            loop {
                select_biased! {
                    event = handle.event_rx.next() => {
                        match event {
                            Some(event) => {
                                if tx.send(event).await.is_err() {
                                    break;
                                }
                            }
                            None => {
                                tx.close_channel();
                                break;
                            }
                        }
                    }
                    chunk = audio_stream.next() => {
                        match chunk {
                            Some(Ok(audio)) => {
                                if let Err(err) = handle.send_audio(audio.to_vec()).await {
                                    if tx.send(Err(err)).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Some(Err(err)) => {
                                if tx.send(Err(DeepgramError::from(
                                    Box::new(err) as Box<dyn Error + Send + Sync + 'static>,
                                ))).await.is_err() {
                                    break;
                                }
                            }
                            None => {
                                // Audio stream ended; close the websocket.
                                if let Err(err) = handle.close().await {
                                    let _ = tx.send(Err(err)).await;
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(AgentStream { rx })
    }
}

// ---------------------------------------------------------------------------
// Internal command type
// ---------------------------------------------------------------------------

enum WsCommand {
    Audio(Vec<u8>),
    Message(String),
    Close,
}

// ---------------------------------------------------------------------------
// AgentHandle
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AgentHandle {
    command_tx: Sender<WsCommand>,
    event_rx: Receiver<Result<AgentEvent>>,
}

impl std::fmt::Debug for WsCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsCommand::Audio(data) => write!(f, "Audio({} bytes)", data.len()),
            WsCommand::Message(msg) => write!(f, "Message({msg})"),
            WsCommand::Close => write!(f, "Close"),
        }
    }
}

impl AgentHandle {
    async fn new(builder: AgentBuilder<'_>) -> Result<AgentHandle> {
        let url = builder.build_url()?;
        let host = url.host_str().ok_or(DeepgramError::InvalidUrl)?;

        let request = {
            let http_builder = Request::builder()
                .method("GET")
                .uri(url.to_string())
                .header("sec-websocket-key", client::generate_key())
                .header("host", host)
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("user-agent", crate::USER_AGENT);

            let http_builder = if let Some(auth) = &builder.deepgram.auth {
                http_builder.header("authorization", auth.header_value())
            } else {
                http_builder
            };
            http_builder.body(())?
        };

        let (ws_stream, _upgrade_response) =
            tokio_tungstenite::connect_async(request).await?;

        let (command_tx, command_rx) = mpsc::channel(256);
        let (event_tx, event_rx) = mpsc::channel(256);

        // Serialize the Settings message to send as the first text frame.
        let settings_json = serde_json::to_string(&AgentClientMessage::Settings(
            SettingsPayload {
                settings: builder.settings,
            },
        ))
        .map_err(DeepgramError::JsonError)?;

        tokio::task::spawn(run_agent_worker(
            ws_stream,
            command_rx,
            event_tx,
            settings_json,
        ));

        Ok(AgentHandle {
            command_tx,
            event_rx,
        })
    }

    /// Send raw audio data to the agent.
    pub async fn send_audio(&mut self, data: Vec<u8>) -> Result<()> {
        self.command_tx
            .send(WsCommand::Audio(data))
            .await
            .map_err(|err| DeepgramError::InternalClientError(err.into()))?;
        Ok(())
    }

    /// Respond to a function call from the agent.
    pub async fn send_function_call_response(
        &mut self,
        function_call_id: &str,
        output: &str,
    ) -> Result<()> {
        self.send_json_message(&AgentClientMessage::FunctionCallResponse {
            function_call_id: function_call_id.to_owned(),
            output: output.to_owned(),
        })
        .await
    }

    /// Update the agent prompt instructions.
    pub async fn update_prompt(&mut self, instructions: &str) -> Result<()> {
        self.send_json_message(&AgentClientMessage::UpdatePrompt {
            instructions: instructions.to_owned(),
        })
        .await
    }

    /// Update speak settings.
    pub async fn update_speak(&mut self, speak: SpeakConfig) -> Result<()> {
        self.send_json_message(&AgentClientMessage::UpdateSpeak { speak })
            .await
    }

    /// Update think settings.
    pub async fn update_think(&mut self, think: ThinkConfig) -> Result<()> {
        self.send_json_message(&AgentClientMessage::UpdateThink { think })
            .await
    }

    /// Inject a user message into the conversation.
    pub async fn inject_user_message(&mut self, content: &str) -> Result<()> {
        self.send_json_message(&AgentClientMessage::InjectUserMessage {
            content: content.to_owned(),
        })
        .await
    }

    /// Inject an agent message into the conversation.
    pub async fn inject_agent_message(&mut self, message: &str) -> Result<()> {
        self.send_json_message(&AgentClientMessage::InjectAgentMessage {
            message: message.to_owned(),
        })
        .await
    }

    /// Send a keep-alive to prevent connection timeout.
    pub async fn keep_alive(&mut self) -> Result<()> {
        self.send_json_message(&AgentClientMessage::KeepAlive {}).await
    }

    /// Close the WebSocket connection.
    pub async fn close(&mut self) -> Result<()> {
        if !self.command_tx.is_closed() {
            self.command_tx
                .send(WsCommand::Close)
                .await
                .map_err(|err| DeepgramError::InternalClientError(err.into()))?;
            self.command_tx.close_channel();
        }
        Ok(())
    }

    /// Receive the next event from the agent.
    pub async fn receive(&mut self) -> Option<Result<AgentEvent>> {
        self.event_rx.next().await
    }

    /// Returns `true` if the command channel is still open.
    pub fn is_connected(&self) -> bool {
        !self.command_tx.is_closed()
    }

    // -- helpers --

    async fn send_json_message(&mut self, msg: &AgentClientMessage) -> Result<()> {
        let json = serde_json::to_string(msg).map_err(DeepgramError::JsonError)?;
        self.command_tx
            .send(WsCommand::Message(json))
            .await
            .map_err(|err| DeepgramError::InternalClientError(err.into()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

async fn run_agent_worker(
    ws_stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    mut command_rx: Receiver<WsCommand>,
    mut event_tx: Sender<Result<AgentEvent>>,
    settings_json: String,
) -> Result<()> {
    let mut partial_frame: Vec<u8> = Vec::new();
    let (mut ws_send, ws_recv) = ws_stream.split();
    let mut ws_recv = ws_recv.fuse();
    let mut is_open: bool = true;

    // Send the Settings message immediately after connecting.
    if let Err(err) = ws_send
        .send(Message::Text(Utf8Bytes::from(settings_json)))
        .await
    {
        let _ = event_tx.send(Err(err.into())).await;
        return Ok(());
    }

    loop {
        select_biased! {
            response = ws_recv.next() => {
                match response {
                    Some(Ok(Message::Text(text))) => {
                        let result: Result<AgentEvent> =
                            serde_json::from_str(&text).map_err(|e| e.into());
                        if event_tx.send(result).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if event_tx
                            .send(Ok(AgentEvent::Audio(data)))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(value))) => {
                        let _ = ws_send.send(Message::Pong(value)).await;
                    }
                    Some(Ok(Message::Close(None))) => {
                        return Ok(());
                    }
                    Some(Ok(Message::Close(Some(closeframe)))) => {
                        return Err(DeepgramError::WebsocketClose {
                            code: closeframe.code.into(),
                            reason: closeframe.reason.to_string(),
                        });
                    }
                    Some(Ok(Message::Frame(frame))) => {
                        match frame.header().opcode {
                            OpCode::Data(Data::Text) => {
                                partial_frame.extend(frame.payload());
                            }
                            OpCode::Data(Data::Continue) => {
                                if !partial_frame.is_empty() {
                                    partial_frame.extend(frame.payload());
                                }
                            }
                            _ => {}
                        }
                        if frame.header().is_final {
                            let data = std::mem::take(&mut partial_frame);
                            let result = serde_json::from_slice(&data).map_err(|e| e.into());
                            if event_tx.send(result).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Ignore pongs.
                    }
                    Some(Err(err)) => {
                        if event_tx.send(Err(err.into())).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        return Ok(());
                    }
                }
            }
            command = command_rx.next() => {
                if is_open {
                    match command {
                        Some(WsCommand::Audio(data)) => {
                            if let Err(err) = ws_send.send(Message::Binary(Bytes::from(data))).await {
                                if event_tx.send(Err(err.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(WsCommand::Message(json)) => {
                            if let Err(err) = ws_send.send(Message::Text(Utf8Bytes::from(json))).await {
                                if event_tx.send(Err(err.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(WsCommand::Close) | None => {
                            let _ = ws_send.send(Message::Close(None)).await;
                            is_open = false;
                        }
                    }
                }
            }
        }
    }

    // Post-loop cleanup
    if is_open {
        let _ = ws_send.send(Message::Close(None)).await;
    }
    event_tx.close_channel();
    while command_rx.next().await.is_some() {
        // Drain remaining commands.
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AgentStream
// ---------------------------------------------------------------------------

#[pin_project]
pub struct AgentStream {
    #[pin]
    rx: Receiver<Result<AgentEvent>>,
}

impl Stream for AgentStream {
    type Item = Result<AgentEvent, DeepgramError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        this.rx.poll_next(cx)
    }
}

impl std::fmt::Debug for AgentStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentStream").finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_url_default() {
        let dg = crate::Deepgram::new("token").unwrap();
        let agent = dg.agent();
        let builder = agent.converse(dummy_settings());
        let url = builder.build_url().unwrap();
        assert_eq!(url.to_string(), "wss://api.deepgram.com/v1/agent/converse");
    }

    #[test]
    fn agent_url_custom_host() {
        let dg =
            crate::Deepgram::with_base_url_and_api_key("http://localhost:8080", "token").unwrap();
        let agent = dg.agent();
        let builder = agent.converse(dummy_settings());
        let url = builder.build_url().unwrap();
        assert_eq!(url.to_string(), "ws://localhost:8080/v1/agent/converse");
    }

    fn dummy_settings() -> AgentSettings {
        use super::super::types::*;
        AgentSettings {
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
                        model: None,
                        instructions: None,
                    },
                    functions: None,
                },
                speak: SpeakConfig { provider: None },
                greeting: None,
                context: None,
            },
            tags: None,
            mip_opt_out: None,
        }
    }
}
