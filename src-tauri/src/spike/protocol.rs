//! Legacy App ID + Access Token protocol spike for Volcengine streaming ASR.
//!
//! Only error classes, timing, and audio-frame counts are emitted as telemetry.
//! Credential values, frame payloads, and transcript text never enter logs.

use std::collections::HashSet;
use std::sync::Once;
use std::time::Duration;

use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{client_async_tls, connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

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
const PROXY_RESPONSE_LIMIT: usize = 8_192;
const INTERNET_SETTINGS_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

static RUSTLS_PROVIDER: Once = Once::new();

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    pub version: u8,
    pub header_words: u8,
    pub message_type: u8,
    pub flags: u8,
    pub serialization: u8,
    pub compression: u8,
}

pub struct Session {
    writer: Writer,
    pending_audio: Vec<u8>,
    pub audio_frames: usize,
}

pub async fn connect(
    credentials: Credentials,
    hotwords: &[String],
) -> Result<(Session, mpsc::Receiver<ServerMessage>), SpikeError> {
    install_rustls_provider();
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
        audio_frames: 0,
    };
    session.send_initial_request(&connect_id, hotwords).await?;
    Ok((session, server_rx))
}

fn install_rustls_provider() {
    RUSTLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
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
        self.send_frame_until(last_frame(), deadline).await
    }

    async fn send_initial_request(
        &mut self,
        connect_id: &str,
        hotwords: &[String],
    ) -> Result<(), SpikeError> {
        self.send_frame(initial_request_frame(connect_id, hotwords)?)
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
        self.send_frame_with_timeout(audio_packet_frame(&packet), timeout)
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

fn initial_request_frame(connect_id: &str, hotwords: &[String]) -> Result<Vec<u8>, SpikeError> {
    let payload = initial_request_payload(connect_id, hotwords);
    let payload = serde_json::to_vec(&payload).map_err(|_| SpikeError::Protocol)?;
    Ok(frame::build(
        MessageType::FullClientRequest,
        Flags::None,
        Serialization::Json,
        &payload,
        None,
    ))
}

fn audio_packet_frame(packet: &[u8]) -> Vec<u8> {
    frame::build(
        MessageType::AudioOnlyRequest,
        Flags::None,
        Serialization::None,
        packet,
        None,
    )
}

fn last_frame() -> Vec<u8> {
    frame::build(
        MessageType::AudioOnlyRequest,
        Flags::LastPacket,
        Serialization::None,
        &[],
        None,
    )
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
    if !matches!(
        parsed.flags,
        flag if flag == Flags::None as u8
            || flag == Flags::HasSequence as u8
            || flag == Flags::LastPacketWithSequence as u8
    ) {
        return Err(SpikeError::Protocol);
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

pub fn inspect_frame_header(bytes: &[u8]) -> Option<FrameHeader> {
    let [first, second, third, ..] = bytes else {
        return None;
    };
    Some(FrameHeader {
        version: first >> 4,
        header_words: first & 0x0f,
        message_type: second >> 4,
        flags: second & 0x0f,
        serialization: third >> 4,
        compression: third & 0x0f,
    })
}

async fn connect_with_retry(credentials: &Credentials, connect_id: &str) -> Result<Ws, SpikeError> {
    let proxy = system_http_proxy();
    for attempt in 1..=CONNECT_ATTEMPTS {
        let request = build_request(credentials, connect_id)?;
        match tokio::time::timeout(CONNECT_TIMEOUT, connect_once(request, proxy.as_deref())).await {
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

async fn connect_once(
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
    proxy: Option<&str>,
) -> Result<
    (
        Ws,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    WsError,
> {
    match proxy {
        Some(proxy) => {
            let stream = open_proxy_tunnel(proxy).await.map_err(WsError::Io)?;
            client_async_tls(request, stream).await
        }
        None => connect_async(request).await,
    }
}

fn system_http_proxy() -> Option<String> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let settings = current_user.open_subkey(INTERNET_SETTINGS_KEY).ok()?;
    let enabled: u32 = settings.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let configured: String = settings.get_value("ProxyServer").ok()?;
    parse_http_proxy(&configured)
}

fn parse_http_proxy(configured: &str) -> Option<String> {
    let entries: Vec<_> = configured
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    let preferred = entries.iter().find_map(|entry| {
        let (scheme, address) = entry.split_once('=')?;
        scheme
            .trim()
            .eq_ignore_ascii_case("https")
            .then(|| address.trim())
    });
    let fallback = entries.iter().find_map(|entry| {
        let (scheme, address) = entry.split_once('=')?;
        scheme
            .trim()
            .eq_ignore_ascii_case("http")
            .then(|| address.trim())
    });
    preferred
        .or(fallback)
        .or_else(|| entries.iter().copied().find(|entry| !entry.contains('=')))
        .and_then(normalize_proxy_address)
}

fn normalize_proxy_address(address: &str) -> Option<String> {
    let address = address
        .trim()
        .strip_prefix("http://")
        .or_else(|| address.trim().strip_prefix("HTTP://"))
        .unwrap_or(address.trim());
    (!address.is_empty() && !address.contains('@') && address.contains(':'))
        .then(|| address.to_owned())
}

async fn open_proxy_tunnel(proxy: &str) -> Result<TcpStream, std::io::Error> {
    let mut stream = TcpStream::connect(proxy).await?;
    stream
        .write_all(
            b"CONNECT openspeech.bytedance.com:443 HTTP/1.1\r\nHost: openspeech.bytedance.com:443\r\nProxy-Connection: Keep-Alive\r\n\r\n",
        )
        .await?;

    let mut response = Vec::with_capacity(512);
    let mut buffer = [0_u8; 512];
    while !response.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "proxy closed CONNECT response",
            ));
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > PROXY_RESPONSE_LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "proxy CONNECT response exceeded limit",
            ));
        }
    }
    if !proxy_tunnel_succeeded(&response) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "proxy rejected CONNECT tunnel",
        ));
    }
    Ok(stream)
}

fn proxy_tunnel_succeeded(response: &[u8]) -> bool {
    let Some(line) = response.split(|byte| *byte == b'\n').next() else {
        return false;
    };
    let mut fields = line
        .trim_ascii_end()
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let _version = fields.next();
    matches!(fields.next(), Some(status) if status == b"200")
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
            Flags::LastPacketWithSequence,
            Serialization::Json,
            payload,
            Some(-4),
        );
        let event = parse_server_frame(&bytes).unwrap().expect("event exists");
        assert!(matches!(event, RecognitionEvent::Final(text) if text == "final-only"));
    }

    #[test]
    fn client_frames_use_documented_flags_without_sequence_data() {
        let initial = initial_request_frame("test-user", &[]).expect("initial frame builds");
        assert_eq!(initial[1], 0x10);
        assert_eq!(
            u32::from_be_bytes(initial[4..8].try_into().unwrap()) as usize,
            initial.len() - 8
        );

        let audio = audio_packet_frame(&vec![0; PCM_PACKET_BYTES]);
        assert_eq!(audio[1], 0x20);
        assert_eq!(audio.len(), 8 + PCM_PACKET_BYTES);
        assert_eq!(
            u32::from_be_bytes(audio[4..8].try_into().unwrap()),
            PCM_PACKET_BYTES as u32
        );

        let last = last_frame();
        assert_eq!(last, vec![0x11, 0x22, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn only_negative_sequence_response_flag_produces_final() {
        let payload = br#"{"result": {"text": "not-final"}}"#;
        let non_final = frame::build(
            MessageType::FullServerResponse,
            Flags::HasSequence,
            Serialization::Json,
            payload,
            Some(-4),
        );
        let event = parse_server_frame(&non_final)
            .unwrap()
            .expect("event exists");
        assert!(matches!(event, RecognitionEvent::Partial(text) if text == "not-final"));

        let invalid = frame::build(
            MessageType::FullServerResponse,
            Flags::LastPacket,
            Serialization::Json,
            payload,
            None,
        );
        assert!(matches!(
            parse_server_frame(&invalid),
            Err(SpikeError::Protocol)
        ));
    }

    #[test]
    fn unsequenced_server_acknowledgement_is_ignored() {
        let acknowledgement = frame::build(
            MessageType::FullServerResponse,
            Flags::None,
            Serialization::Json,
            br#"{"code": 1000}"#,
            None,
        );
        assert!(parse_server_frame(&acknowledgement)
            .expect("acknowledgement is valid")
            .is_none());
    }

    #[test]
    fn rustls_crypto_provider_is_initialized() {
        install_rustls_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn header_inspection_excludes_payload() {
        let header = inspect_frame_header(&[0x11, 0x93, 0x10, 0x00, b's', b'e', b'c'])
            .expect("header is present");
        assert_eq!(header.version, 1);
        assert_eq!(header.header_words, 1);
        assert_eq!(header.message_type, MessageType::FullServerResponse as u8);
        assert_eq!(header.flags, Flags::LastPacketWithSequence as u8);
        assert_eq!(header.serialization, Serialization::Json as u8);
        assert_eq!(header.compression, 0);
        assert!(inspect_frame_header(&[0x11, 0x93]).is_none());
    }

    #[test]
    fn proxy_configuration_prefers_https_over_http() {
        assert_eq!(
            parse_http_proxy("http=127.0.0.1:8080; https=127.0.0.1:7890"),
            Some("127.0.0.1:7890".to_owned())
        );
        assert_eq!(
            parse_http_proxy("http://127.0.0.1:7890"),
            Some("127.0.0.1:7890".to_owned())
        );
        assert_eq!(parse_http_proxy("https=user:secret@127.0.0.1:7890"), None);
    }

    #[test]
    fn only_successful_connect_responses_open_a_proxy_tunnel() {
        assert!(proxy_tunnel_succeeded(
            b"HTTP/1.1 200 Connection established\r\n\r\n"
        ));
        assert!(proxy_tunnel_succeeded(b"HTTP/1.0 200 OK\r\n\r\n"));
        assert!(!proxy_tunnel_succeeded(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"
        ));
        assert!(!proxy_tunnel_succeeded(b"HTTP/1.1 2000 Invalid\r\n\r\n"));
        assert!(!proxy_tunnel_succeeded(b"not an HTTP response\r\n\r\n"));
    }
}
