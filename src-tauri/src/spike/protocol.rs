//! Legacy App ID + Access Token protocol spike for Volcengine streaming ASR.
//!
//! Only error classes, timing, and audio-frame counts are emitted as telemetry.
//! Credential values, frame payloads, and transcript text never enter logs.

use std::collections::HashSet;
use std::time::Duration;

use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
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
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const FINALIZE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_QUEUE_CAPACITY: usize = 32;

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
) -> Result<(Session, mpsc::Receiver<ServerMessage>), SpikeError> {
    let connect_id = Uuid::new_v4().to_string();
    let websocket = connect_with_retry(&credentials, &connect_id).await?;
    let (writer, mut reader) = websocket.split();
    let (server_tx, server_rx) = mpsc::channel(SERVER_QUEUE_CAPACITY);

    tokio::spawn(async move {
        while let Some(message) = reader.next().await {
            match message {
                Ok(Message::Binary(bytes)) => {
                    if server_tx.send(ServerMessage::Frame(bytes)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = server_tx.send(ServerMessage::Closed).await;
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = server_tx.send(ServerMessage::NetworkFailed).await;
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
        self.send_audio_with_deadline(pcm, None).await
    }

    pub async fn send_audio_until(
        &mut self,
        pcm: &[u8],
        deadline: Instant,
    ) -> Result<(), SpikeError> {
        self.send_audio_with_deadline(pcm, Some(deadline)).await
    }

    async fn send_audio_with_deadline(
        &mut self,
        pcm: &[u8],
        deadline: Option<Instant>,
    ) -> Result<(), SpikeError> {
        self.pending_audio.extend_from_slice(pcm);
        while self.pending_audio.len() >= PCM_PACKET_BYTES {
            let packet: Vec<u8> = self.pending_audio.drain(..PCM_PACKET_BYTES).collect();
            match deadline {
                Some(deadline) => self.send_audio_packet_until(packet, deadline).await?,
                None => self.send_audio_packet(packet).await?,
            }
        }
        Ok(())
    }

    pub async fn send_last_frame(&mut self) -> Result<(), SpikeError> {
        self.send_last_frame_until(Instant::now() + FINALIZE_WRITE_TIMEOUT)
            .await
    }

    pub async fn send_last_frame_until(&mut self, deadline: Instant) -> Result<(), SpikeError> {
        if !self.pending_audio.is_empty() {
            let packet = std::mem::take(&mut self.pending_audio);
            self.send_audio_packet_until(packet, deadline).await?;
        }
        let final_sequence = -self.next_sequence;
        self.next_sequence += 1;
        self.send_frame_until(
            frame::build(
                MessageType::AudioOnlyRequest,
                Flags::NegativeSequence,
                Serialization::None,
                &[],
                Some(final_sequence),
            ),
            deadline,
        )
        .await
    }

    async fn send_initial_request(
        &mut self,
        connect_id: &str,
        hotwords: &[String],
    ) -> Result<(), SpikeError> {
        let payload = initial_request_payload(connect_id, hotwords);
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
        self.send_audio_packet_with_timeout(packet, WRITE_TIMEOUT)
            .await
    }

    async fn send_audio_packet_until(
        &mut self,
        packet: Vec<u8>,
        deadline: Instant,
    ) -> Result<(), SpikeError> {
        self.send_audio_packet_with_timeout(packet, remaining_until(deadline)?)
            .await
    }

    async fn send_audio_packet_with_timeout(
        &mut self,
        packet: Vec<u8>,
        timeout: Duration,
    ) -> Result<(), SpikeError> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.send_frame_with_timeout(
            frame::build(
                MessageType::AudioOnlyRequest,
                Flags::PositiveSequence,
                Serialization::None,
                &packet,
                Some(sequence),
            ),
            timeout,
        )
        .await?;
        self.audio_frames += 1;
        Ok(())
    }

    async fn send_frame(&mut self, bytes: Vec<u8>) -> Result<(), SpikeError> {
        self.send_frame_with_timeout(bytes, WRITE_TIMEOUT).await
    }

    async fn send_frame_until(
        &mut self,
        bytes: Vec<u8>,
        deadline: Instant,
    ) -> Result<(), SpikeError> {
        self.send_frame_with_timeout(bytes, remaining_until(deadline)?)
            .await
    }

    async fn send_frame_with_timeout(
        &mut self,
        bytes: Vec<u8>,
        timeout: Duration,
    ) -> Result<(), SpikeError> {
        send_with_timeout(&mut self.writer, Message::Binary(bytes), timeout).await
    }
}

async fn send_with_timeout<S>(
    sink: &mut S,
    message: Message,
    timeout: Duration,
) -> Result<(), SpikeError>
where
    S: Sink<Message> + Unpin,
{
    if timeout.is_zero() {
        return Err(SpikeError::Network);
    }
    match tokio::time::timeout(timeout, sink.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(SpikeError::Network),
    }
}

fn remaining_until(deadline: Instant) -> Result<Duration, SpikeError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(SpikeError::Network);
    }
    Ok(remaining)
}

fn initial_request_payload(connect_id: &str, hotwords: &[String]) -> Value {
    let mut request = json!({
        "model_name": "bigmodel",
        "enable_itn": true,
        "enable_punc": true,
        "show_utterances": true,
        "enable_nonstream": true
    });
    if let Some(context) = hotword_context(hotwords) {
        request["corpus"] = json!({ "context": context });
    }
    json!({
        "user": { "uid": connect_id },
        "audio": {
            "format": "pcm",
            "rate": 16000,
            "bits": 16,
            "channel": 1,
            "codec": "raw"
        },
        "request": request
    })
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
    fn request_uses_corpus_context_and_required_dual_pass_options() {
        let payload = initial_request_payload("test-user", &["area".to_owned()]);
        assert_eq!(payload["request"]["enable_nonstream"], true);
        assert!(payload.pointer("/request/context").is_none());
        let context = payload
            .pointer("/request/corpus/context")
            .and_then(Value::as_str)
            .expect("context exists under request.corpus");
        let parsed: Value = serde_json::from_str(context).expect("context is JSON");
        assert_eq!(parsed["hotwords"][0]["word"], "area");
    }

    #[tokio::test]
    async fn write_timeout_is_a_network_error() {
        use std::convert::Infallible;

        let mut pending_sink = futures_util::sink::unfold((), |_, _: Message| {
            futures_util::future::pending::<Result<(), Infallible>>()
        });
        let error = send_with_timeout(
            &mut pending_sink,
            Message::Binary(Vec::new()),
            Duration::from_millis(1),
        )
        .await
        .expect_err("pending writer must time out");
        assert!(matches!(error, SpikeError::Network));
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
