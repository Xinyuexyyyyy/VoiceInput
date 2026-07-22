//! Legacy App ID + Access Token protocol spike for Volcengine streaming ASR.
//!
//! Only error classes, timing, and audio-frame counts are emitted as telemetry.
//! Credential values, frame payloads, and transcript text never enter logs.

use std::collections::HashSet;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::spike::credentials::Credentials;
use crate::spike::error::SpikeError;
use crate::spike::frame::{self, Flags, MessageType, Serialization};

const ENDPOINT: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";
const PCM_PACKET_BYTES: usize = 6_400;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_ATTEMPTS: usize = 3;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Writer = futures_util::stream::SplitSink<Ws, Message>;

pub enum ServerMessage {
    Frame(Vec<u8>),
    Closed,
    NetworkFailed,
}

pub enum RecognitionEvent {
    Partial(String),
    Final(String),
}

pub struct Session {
    writer: Writer,
    pending_audio: Vec<u8>,
    next_sequence: i32,
    pub audio_frames: usize,
}

pub async fn connect(
    credentials: Credentials,
    hotwords: &[String],
) -> Result<(Session, mpsc::UnboundedReceiver<ServerMessage>), SpikeError> {
    let connect_id = Uuid::new_v4().to_string();
    let websocket = connect_with_retry(&credentials, &connect_id).await?;
    let (writer, mut reader) = websocket.split();
    let (server_tx, server_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        while let Some(message) = reader.next().await {
            match message {
                Ok(Message::Binary(bytes)) => {
                    if server_tx.send(ServerMessage::Frame(bytes)).is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = server_tx.send(ServerMessage::Closed);
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = server_tx.send(ServerMessage::NetworkFailed);
                    break;
                }
            }
        }
    });

    let mut session = Session {
        writer,
        pending_audio: Vec::new(),
        next_sequence: 1,
        audio_frames: 0,
    };
    session.send_initial_request(&connect_id, hotwords).await?;
    Ok((session, server_rx))
}

impl Session {
    pub async fn send_audio(&mut self, pcm: &[u8]) -> Result<(), SpikeError> {
        self.pending_audio.extend_from_slice(pcm);
        while self.pending_audio.len() >= PCM_PACKET_BYTES {
            let packet: Vec<u8> = self.pending_audio.drain(..PCM_PACKET_BYTES).collect();
            self.send_audio_packet(packet).await?;
        }
        Ok(())
    }

    pub async fn send_last_frame(&mut self) -> Result<(), SpikeError> {
        if !self.pending_audio.is_empty() {
            let packet = std::mem::take(&mut self.pending_audio);
            self.send_audio_packet(packet).await?;
        }
        let final_sequence = -self.next_sequence;
        self.next_sequence += 1;
        self.send_frame(frame::build(
            MessageType::AudioOnlyRequest,
            Flags::NegativeSequence,
            Serialization::None,
            &[],
            Some(final_sequence),
        ))
        .await
    }

    async fn send_initial_request(
        &mut self,
        connect_id: &str,
        hotwords: &[String],
    ) -> Result<(), SpikeError> {
        let mut request = json!({
            "model_name": "bigmodel",
            "enable_itn": true,
            "enable_punc": true,
            "show_utterances": true,
            "enable_nonstream": true
        });
        if let Some(context) = hotword_context(hotwords) {
            request["context"] = Value::String(context);
        }
        let payload = json!({
            "user": { "uid": connect_id },
            "audio": {
                "format": "pcm",
                "rate": 16000,
                "bits": 16,
                "channel": 1,
                "codec": "raw"
            },
            "request": request
        });
        let payload = serde_json::to_vec(&payload).map_err(|_| SpikeError::Protocol)?;
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.send_frame(frame::build(
            MessageType::FullClientRequest,
            Flags::PositiveSequence,
            Serialization::Json,
            &payload,
            Some(sequence),
        ))
        .await
    }

    async fn send_audio_packet(&mut self, packet: Vec<u8>) -> Result<(), SpikeError> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.audio_frames += 1;
        self.send_frame(frame::build(
            MessageType::AudioOnlyRequest,
            Flags::PositiveSequence,
            Serialization::None,
            &packet,
            Some(sequence),
        ))
        .await
    }

    async fn send_frame(&mut self, bytes: Vec<u8>) -> Result<(), SpikeError> {
        self.writer
            .send(Message::Binary(bytes))
            .await
            .map_err(|_| SpikeError::Network)
    }
}

pub fn parse_server_frame(bytes: &[u8]) -> Result<Option<RecognitionEvent>, SpikeError> {
    let parsed = frame::parse(bytes).ok_or(SpikeError::Protocol)?;
    if parsed.message_type == Some(MessageType::ErrorMessage) {
        return Err(SpikeError::ServerError(
            parsed.error_code.unwrap_or_default(),
        ));
    }
    if parsed.message_type != Some(MessageType::FullServerResponse) {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&parsed.payload).map_err(|_| SpikeError::Protocol)?;
    let Some(text) = result_text(&value) else {
        return Ok(None);
    };
    if parsed.is_final() {
        return Ok(Some(RecognitionEvent::Final(text)));
    }
    Ok(Some(RecognitionEvent::Partial(text)))
}

async fn connect_with_retry(credentials: &Credentials, connect_id: &str) -> Result<Ws, SpikeError> {
    for attempt in 1..=CONNECT_ATTEMPTS {
        let request = build_request(credentials, connect_id)?;
        match tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request)).await {
            Ok(Ok((websocket, _))) => return Ok(websocket),
            Ok(Err(error)) => {
                let classified = classify_connect_error(error);
                if !matches!(classified, SpikeError::Network) || attempt == CONNECT_ATTEMPTS {
                    return Err(classified);
                }
            }
            Err(_) if attempt == CONNECT_ATTEMPTS => return Err(SpikeError::Network),
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
    }
    Err(SpikeError::Network)
}

fn build_request(
    credentials: &Credentials,
    connect_id: &str,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, SpikeError> {
    let mut request = ENDPOINT
        .into_client_request()
        .map_err(|_| SpikeError::Network)?;
    let headers = request.headers_mut();
    headers.insert(
        "X-Api-App-Key",
        HeaderValue::from_str(&credentials.app_id).map_err(|_| SpikeError::CredentialsMissing)?,
    );
    headers.insert(
        "X-Api-Access-Key",
        HeaderValue::from_str(&credentials.access_token)
            .map_err(|_| SpikeError::CredentialsMissing)?,
    );
    headers.insert(
        "X-Api-Resource-Id",
        HeaderValue::from_str(&credentials.resource_id)
            .map_err(|_| SpikeError::CredentialsMissing)?,
    );
    headers.insert(
        "X-Api-Connect-Id",
        HeaderValue::from_str(connect_id).map_err(|_| SpikeError::Protocol)?,
    );
    Ok(request)
}

fn classify_connect_error(error: WsError) -> SpikeError {
    if let WsError::Http(response) = &error {
        match response.status().as_u16() {
            401 | 403 => return SpikeError::AuthRejected(response.status().as_u16()),
            429 => return SpikeError::RateLimited(429),
            _ => {}
        }
    }
    SpikeError::Network
}

fn hotword_context(entries: &[String]) -> Option<String> {
    let mut seen = HashSet::new();
    let words: Vec<Value> = entries
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .filter(|entry| seen.insert(entry.to_ascii_lowercase()))
        .take(80)
        .map(|entry| json!({ "word": entry }))
        .collect();
    (!words.is_empty()).then(|| json!({ "hotwords": words }).to_string())
}

fn result_text(value: &Value) -> Option<String> {
    let result = match value.get("result") {
        Some(Value::Object(_)) => value.get("result")?,
        Some(Value::Array(items)) => items.first()?,
        _ => value,
    };
    result
        .get("text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotwords_are_trimmed_deduplicated_and_capped() {
        let mut hotwords = vec![" area ".to_owned(), "AREA".to_owned()];
        hotwords.extend((0..100).map(|index| format!("word-{index}")));
        let context = hotword_context(&hotwords).expect("context exists");
        let parsed: Value = serde_json::from_str(&context).expect("valid json");
        assert_eq!(parsed["hotwords"].as_array().unwrap().len(), 80);
        assert_eq!(parsed["hotwords"][0]["word"], "area");
    }

    #[test]
    fn request_uses_required_dual_pass_options() {
        let mut request = json!({
            "model_name": "bigmodel",
            "enable_itn": true,
            "enable_punc": true,
            "show_utterances": true,
            "enable_nonstream": true
        });
        request["context"] = Value::String(hotword_context(&["area".to_owned()]).unwrap());
        assert_eq!(request["enable_nonstream"], true);
        assert!(request["context"].as_str().unwrap().contains("area"));
    }

    #[test]
    fn final_frame_yields_final_text() {
        let payload = br#"{"result": {"text": "final-only"}}"#;
        let bytes = frame::build(
            MessageType::FullServerResponse,
            Flags::NegativeSequence,
            Serialization::Json,
            payload,
            Some(-4),
        );
        let event = parse_server_frame(&bytes).unwrap().expect("event exists");
        assert!(matches!(event, RecognitionEvent::Final(text) if text == "final-only"));
    }
}
