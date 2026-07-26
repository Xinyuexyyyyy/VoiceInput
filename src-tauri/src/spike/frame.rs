//! Volcengine binary frame codec adapted from the MIT-licensed OpenLess project.

const HEADER_BYTE_0: u8 = 0x11;
const COMPRESSION_NONE: u8 = 0;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageType {
    FullClientRequest = 0b0001,
    AudioOnlyRequest = 0b0010,
    FullServerResponse = 0b1001,
    ErrorMessage = 0b1111,
}

impl MessageType {
    fn from_raw(value: u8) -> Option<Self> {
        match value {
            0b0001 => Some(Self::FullClientRequest),
            0b0010 => Some(Self::AudioOnlyRequest),
            0b1001 => Some(Self::FullServerResponse),
            0b1111 => Some(Self::ErrorMessage),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Flags {
    None = 0,
    HasSequence = 1,
    LastPacket = 2,
    LastPacketWithSequence = 3,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Serialization {
    None = 0,
    Json = 1,
}

pub struct ParsedFrame {
    pub message_type: Option<MessageType>,
    pub flags: u8,
    pub error_code: Option<u32>,
    pub payload: Vec<u8>,
}

impl ParsedFrame {
    pub fn is_final(&self) -> bool {
        self.flags == Flags::LastPacketWithSequence as u8
    }
}

pub fn build(
    message_type: MessageType,
    flags: Flags,
    serialization: Serialization,
    payload: &[u8],
    sequence: Option<i32>,
) -> Vec<u8> {
    let mut result = Vec::with_capacity(12 + payload.len());
    result.push(HEADER_BYTE_0);
    result.push(((message_type as u8) << 4) | flags as u8);
    result.push((serialization as u8) << 4 | COMPRESSION_NONE);
    result.push(0);

    if matches!(flags, Flags::HasSequence | Flags::LastPacketWithSequence) {
        result.extend_from_slice(&sequence.unwrap_or_default().to_be_bytes());
    }

    result.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    result.extend_from_slice(payload);
    result
}

pub fn parse(data: &[u8]) -> Option<ParsedFrame> {
    if data.len() < 8 {
        return None;
    }
    let header_size = usize::from(data[0] & 0x0f) * 4;
    if header_size < 4 || data.len() < header_size + 4 || data[2] & 0x0f != COMPRESSION_NONE {
        return None;
    }

    let message_type = MessageType::from_raw(data[1] >> 4);
    let flags = data[1] & 0x0f;
    let mut offset = header_size;
    if matches!(flags, 1 | 3) {
        read_i32(data, offset)?;
        offset += 4;
    }

    if message_type == Some(MessageType::ErrorMessage) {
        let error_code = read_u32(data, offset)?;
        let size = read_u32(data, offset + 4)? as usize;
        offset += 8;
        return data.get(offset..offset + size).map(|payload| ParsedFrame {
            message_type,
            flags,
            error_code: Some(error_code),
            payload: payload.to_vec(),
        });
    }

    let size = read_u32(data, offset)? as usize;
    offset += 4;
    data.get(offset..offset + size).map(|payload| ParsedFrame {
        message_type,
        flags,
        error_code: None,
        payload: payload.to_vec(),
    })
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
}

fn read_i32(data: &[u8], offset: usize) -> Option<i32> {
    read_u32(data, offset).map(|value| value as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_last_response_flag_is_final() {
        let frame = build(
            MessageType::FullServerResponse,
            Flags::LastPacketWithSequence,
            Serialization::None,
            &[],
            Some(-3),
        );
        let parsed = parse(&frame).expect("frame parses");
        assert!(parsed.is_final());
    }

    #[test]
    fn negative_sequence_value_without_last_flag_is_not_final() {
        let frame = build(
            MessageType::FullServerResponse,
            Flags::HasSequence,
            Serialization::Json,
            b"{}",
            Some(-3),
        );
        let parsed = parse(&frame).expect("frame parses");
        assert!(!parsed.is_final());
    }

    #[test]
    fn rejects_compressed_frame() {
        let mut frame = build(
            MessageType::FullServerResponse,
            Flags::None,
            Serialization::Json,
            b"{}",
            None,
        );
        frame[2] |= 1;
        assert!(parse(&frame).is_none());
    }
}
