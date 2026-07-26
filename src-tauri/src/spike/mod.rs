mod audio;
mod credentials;
mod error;
mod frame;
mod protocol;
mod safe_log;

pub use audio::{Recorder, RecorderFailure};
pub use credentials::{Credentials, DEFAULT_RESOURCE_ID};
pub use error::{ErrorClass, SpikeError};
pub use protocol::{
    connect, inspect_frame_header, parse_server_frame, FrameHeader, RecognitionEvent,
    ServerMessage, Session,
};
pub use safe_log::{SafeEvent, SpikePhase};
