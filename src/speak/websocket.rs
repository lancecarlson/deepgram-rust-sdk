//! WebSocket-based live text-to-speech.
//!
//! See the [Deepgram TTS WebSocket API Reference][api] for more info.
//!
//! [api]: https://developers.deepgram.com/docs/tts-websocket

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    stream::StreamExt,
    SinkExt, Stream,
};
use http::Request;
use pin_project::pin_project;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};
use tungstenite::{handshake::client, Utf8Bytes};
use url::Url;

use super::options::{Encoding, Model};
use crate::{Deepgram, DeepgramError, Result, Speak};

static LIVE_SPEAK_URL_PATH: &str = "v1/speak";

/// Events received from the Deepgram TTS WebSocket server.
#[derive(Debug, Clone)]
pub enum SpeakEvent {
    /// Raw audio data in the configured encoding/sample_rate.
    Audio(Bytes),
    /// Confirmation that all audio for the current text has been sent.
    Flushed,
    /// Confirmation that a barge-in (clear) was processed.
    Cleared,
    /// Metadata about the TTS session/model.
    Metadata {
        /// The unique request identifier.
        request_id: String,
        /// The model name used for synthesis.
        model_name: String,
        /// The model version.
        model_version: String,
        /// The model UUID.
        model_uuid: String,
    },
    /// A non-fatal warning from the server.
    Warning {
        /// Warning code.
        code: String,
        /// Warning message.
        message: String,
    },
    /// A fatal error from the server.
    Error {
        /// Error code.
        code: String,
        /// Error message.
        message: String,
    },
}

/// JSON messages received from the TTS WebSocket server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ServerMessage {
    Flushed {},
    Cleared {},
    Metadata {
        request_id: String,
        model_name: String,
        model_version: String,
        model_uuid: String,
    },
    Warning {
        warn_code: String,
        warn_msg: String,
    },
    Error {
        err_code: String,
        err_msg: String,
    },
}

/// JSON messages sent to the TTS WebSocket server.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum ClientMessage {
    Speak { text: String },
    Flush {},
    Clear {},
    Close {},
}

/// Internal messages sent from the handle to the worker task.
#[derive(Debug)]
enum WsCommand {
    Message(ClientMessage),
}

/// Builder for configuring a live TTS WebSocket connection.
///
/// Constructed via [`Speak::live`].
#[derive(Debug)]
pub struct SpeakWebsocketBuilder<'a> {
    deepgram: &'a Deepgram,
    model: Option<Model>,
    encoding: Option<Encoding>,
    sample_rate: Option<u32>,
}

/// A handle to control a live TTS WebSocket connection.
///
/// Use this for low-level control over the TTS stream: sending text,
/// flushing, clearing (barge-in), and receiving audio events.
#[derive(Debug)]
pub struct SpeakWebsocketHandle {
    command_tx: Sender<WsCommand>,
    event_rx: Receiver<Result<SpeakEvent>>,
}

/// A stream of [`SpeakEvent`]s from the TTS WebSocket server.
#[derive(Debug)]
#[pin_project]
pub struct SpeakStream {
    #[pin]
    rx: Receiver<Result<SpeakEvent>>,
}

impl Speak<'_> {
    /// Begin configuring a live TTS WebSocket connection.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let dg = deepgram::Deepgram::new("api_key")?;
    /// let mut handle = dg.text_to_speech().live()
    ///     .model(deepgram::speak::options::Model::CustomId("aura-2-thalia-en".into()))
    ///     .encoding(deepgram::speak::options::Encoding::Mulaw)
    ///     .sample_rate(8000)
    ///     .handle()
    ///     .await?;
    ///
    /// handle.send_text("Hello, world!").await?;
    /// handle.flush().await?;
    ///
    /// while let Some(event) = handle.receive().await {
    ///     match event? {
    ///         deepgram::speak::websocket::SpeakEvent::Audio(bytes) => {
    ///             // Process audio chunk
    ///         }
    ///         deepgram::speak::websocket::SpeakEvent::Flushed => {
    ///             // All audio for current text sent
    ///             break;
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn live(&self) -> SpeakWebsocketBuilder<'_> {
        SpeakWebsocketBuilder {
            deepgram: self.0,
            model: None,
            encoding: None,
            sample_rate: None,
        }
    }
}

impl<'a> SpeakWebsocketBuilder<'a> {
    /// Set the TTS model.
    pub fn model(mut self, model: Model) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the audio encoding for the output.
    pub fn encoding(mut self, encoding: Encoding) -> Self {
        self.encoding = Some(encoding);
        self
    }

    /// Set the sample rate for the output audio.
    pub fn sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = Some(sample_rate);
        self
    }

    /// Build the WebSocket URL with query parameters.
    fn build_url(&self) -> Result<Url> {
        let mut url = self
            .deepgram
            .base_url
            .join(LIVE_SPEAK_URL_PATH)
            .map_err(|_| DeepgramError::InvalidUrl)?;

        // Convert scheme to ws/wss
        match url.scheme() {
            "http" | "ws" => url
                .set_scheme("ws")
                .expect("valid scheme conversion"),
            "https" | "wss" => url
                .set_scheme("wss")
                .expect("valid scheme conversion"),
            _ => return Err(DeepgramError::InvalidUrl),
        }

        // Add query parameters
        let has_params = self.model.is_some() || self.encoding.is_some() || self.sample_rate.is_some();
        if has_params {
            let mut pairs = url.query_pairs_mut();
            if let Some(model) = &self.model {
                pairs.append_pair("model", model.as_ref());
            }
            if let Some(encoding) = &self.encoding {
                pairs.append_pair("encoding", encoding.as_str());
            }
            if let Some(sample_rate) = &self.sample_rate {
                pairs.append_pair("sample_rate", &sample_rate.to_string());
            }
        }

        Ok(url)
    }

    /// Connect and return a low-level handle for direct control.
    pub async fn handle(self) -> Result<SpeakWebsocketHandle> {
        SpeakWebsocketHandle::new(self).await
    }

    /// Connect and return a stream of [`SpeakEvent`]s, feeding text from
    /// the provided stream. Each text item is sent followed by a flush.
    pub async fn stream<S>(self, text_stream: S) -> Result<SpeakStream>
    where
        S: Stream<Item = String> + Send + Unpin + 'static,
    {
        let mut handle = self.handle().await?;

        let (tx, rx) = mpsc::channel(256);
        tokio::task::spawn(async move {
            let mut tx = tx;
            let mut text_stream = text_stream.fuse();

            loop {
                futures::select_biased! {
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
                    text = text_stream.next() => {
                        match text {
                            Some(text) => {
                                if let Err(err) = handle.send_text(&text).await {
                                    let _ = tx.send(Err(err)).await;
                                    break;
                                }
                                if let Err(err) = handle.flush().await {
                                    let _ = tx.send(Err(err)).await;
                                    break;
                                }
                            }
                            None => {
                                // Text stream finished, close the connection
                                let _ = handle.close().await;
                            }
                        }
                    }
                }
            }
        });

        Ok(SpeakStream { rx })
    }
}

impl SpeakWebsocketHandle {
    async fn new(builder: SpeakWebsocketBuilder<'_>) -> Result<SpeakWebsocketHandle> {
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

            let builder = if let Some(auth) = &builder.deepgram.auth {
                http_builder.header("authorization", auth.header_value())
            } else {
                http_builder
            };
            builder.body(())?
        };

        let (ws_stream, _upgrade_response) = tokio_tungstenite::connect_async(request).await?;

        let (command_tx, command_rx) = mpsc::channel(256);
        let (event_tx, event_rx) = mpsc::channel(256);

        tokio::task::spawn(run_worker(ws_stream, command_rx, event_tx));

        Ok(SpeakWebsocketHandle {
            command_tx,
            event_rx,
        })
    }

    /// Send text to be synthesized into speech.
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        self.send_client_message(ClientMessage::Speak {
            text: text.to_string(),
        })
        .await
    }

    /// Signal end of current text. The server will send remaining audio
    /// then a [`SpeakEvent::Flushed`] event.
    pub async fn flush(&mut self) -> Result<()> {
        self.send_client_message(ClientMessage::Flush {}).await
    }

    /// Interrupt current speech and discard buffered audio (barge-in).
    pub async fn clear(&mut self) -> Result<()> {
        self.send_client_message(ClientMessage::Clear {}).await
    }

    /// Gracefully close the WebSocket connection.
    pub async fn close(&mut self) -> Result<()> {
        let result = self.send_client_message(ClientMessage::Close {}).await;
        self.command_tx.close_channel();
        result
    }

    /// Receive the next event from the server.
    ///
    /// Returns `None` when the connection is closed.
    pub async fn receive(&mut self) -> Option<Result<SpeakEvent>> {
        self.event_rx.next().await
    }

    /// Check if the connection is still open.
    ///
    /// Returns `true` if commands can still be sent.
    pub fn is_connected(&self) -> bool {
        !self.command_tx.is_closed()
    }

    async fn send_client_message(&mut self, msg: ClientMessage) -> Result<()> {
        self.command_tx
            .send(WsCommand::Message(msg))
            .await
            .map_err(|err| DeepgramError::InternalClientError(err.into()))?;
        Ok(())
    }
}

async fn run_worker(
    ws_stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    mut command_rx: Receiver<WsCommand>,
    mut event_tx: Sender<Result<SpeakEvent>>,
) -> Result<()> {
    let (mut ws_send, ws_recv) = ws_stream.split();
    let mut ws_recv = ws_recv.fuse();

    loop {
        futures::select_biased! {
            response = ws_recv.next() => {
                match response {
                    Some(Ok(Message::Binary(data))) => {
                        // Binary frames are raw audio data
                        if event_tx.send(Ok(SpeakEvent::Audio(data))).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(ServerMessage::Flushed {}) => {
                                if event_tx.send(Ok(SpeakEvent::Flushed)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Warning { warn_code, warn_msg }) => {
                                if event_tx.send(Ok(SpeakEvent::Warning {
                                    code: warn_code,
                                    message: warn_msg,
                                })).await.is_err() {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Error { err_code, err_msg }) => {
                                if event_tx.send(Ok(SpeakEvent::Error {
                                    code: err_code,
                                    message: err_msg,
                                })).await.is_err() {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Cleared {}) => {
                                if event_tx.send(Ok(SpeakEvent::Cleared)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Metadata { request_id, model_name, model_version, model_uuid }) => {
                                if event_tx.send(Ok(SpeakEvent::Metadata {
                                    request_id,
                                    model_name,
                                    model_version,
                                    model_uuid,
                                })).await.is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                if event_tx.send(Err(err.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(value))) => {
                        let _ = ws_send.send(Message::Pong(value)).await;
                    }
                    Some(Ok(Message::Close(_))) => {
                        break;
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {
                        // Ignore
                    }
                    Some(Err(err)) => {
                        let _ = event_tx.send(Err(err.into())).await;
                        break;
                    }
                    None => {
                        // Connection closed
                        break;
                    }
                }
            }
            command = command_rx.next() => {
                match command {
                    Some(WsCommand::Message(msg)) => {
                        let json = serde_json::to_string(&msg).unwrap_or_default();
                        if let Err(err) = ws_send.send(Message::Text(Utf8Bytes::from(json))).await {
                            let _ = event_tx.send(Err(err.into())).await;
                            break;
                        }
                    }
                    None => {
                        // Handle dropped, close the connection
                        let _ = ws_send.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
    }

    event_tx.close_channel();
    // Drain remaining commands
    while command_rx.next().await.is_some() {}
    Ok(())
}

impl Stream for SpeakStream {
    type Item = Result<SpeakEvent, DeepgramError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        this.rx.poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speak_ws_url() {
        let dg = crate::Deepgram::new("token").unwrap();
        let speak = dg.text_to_speech();
        let builder = speak.live();
        let url = builder.build_url().unwrap();
        assert_eq!(url.to_string(), "wss://api.deepgram.com/v1/speak");
    }

    #[test]
    fn test_speak_ws_url_with_params() {
        let dg = crate::Deepgram::new("token").unwrap();
        let speak = dg.text_to_speech();
        let builder = speak
            .live()
            .model(Model::CustomId("aura-2-thalia-en".into()))
            .encoding(Encoding::Mulaw)
            .sample_rate(8000);
        let url = builder.build_url().unwrap();
        assert_eq!(
            url.to_string(),
            "wss://api.deepgram.com/v1/speak?model=aura-2-thalia-en&encoding=mulaw&sample_rate=8000"
        );
    }

    #[test]
    fn test_speak_ws_url_custom_host() {
        let dg =
            crate::Deepgram::with_base_url_and_api_key("http://localhost:8080", "token").unwrap();
        let speak = dg.text_to_speech();
        let builder = speak.live();
        let url = builder.build_url().unwrap();
        assert_eq!(url.to_string(), "ws://localhost:8080/v1/speak");
    }

    #[test]
    fn test_client_message_serialization() {
        let msg = ClientMessage::Speak {
            text: "Hello".into(),
        };
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"Speak","text":"Hello"}"#
        );

        let msg = ClientMessage::Flush {};
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"Flush"}"#
        );

        let msg = ClientMessage::Clear {};
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"Clear"}"#
        );

        let msg = ClientMessage::Close {};
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"Close"}"#
        );
    }

    #[test]
    fn test_server_message_deserialization() {
        let msg: ServerMessage = serde_json::from_str(r#"{"type":"Flushed"}"#).unwrap();
        assert!(matches!(msg, ServerMessage::Flushed {}));

        let msg: ServerMessage =
            serde_json::from_str(r#"{"type":"Warning","warn_code":"1001","warn_msg":"test"}"#)
                .unwrap();
        assert!(matches!(msg, ServerMessage::Warning { .. }));

        let msg: ServerMessage =
            serde_json::from_str(r#"{"type":"Error","err_code":"2001","err_msg":"fatal"}"#)
                .unwrap();
        assert!(matches!(msg, ServerMessage::Error { .. }));
    }

    #[test]
    fn test_cleared_deserialization() {
        let msg: ServerMessage = serde_json::from_str(r#"{"type":"Cleared"}"#).unwrap();
        assert!(matches!(msg, ServerMessage::Cleared {}));
    }

    #[test]
    fn test_metadata_deserialization() {
        let json = r#"{"type":"Metadata","request_id":"abc-123","model_name":"Aura","model_version":"2024-01","model_uuid":"uuid-456"}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            ServerMessage::Metadata { request_id, model_name, model_version, model_uuid } => {
                assert_eq!(request_id, "abc-123");
                assert_eq!(model_name, "Aura");
                assert_eq!(model_version, "2024-01");
                assert_eq!(model_uuid, "uuid-456");
            }
            _ => panic!("expected Metadata variant"),
        }
    }
}
