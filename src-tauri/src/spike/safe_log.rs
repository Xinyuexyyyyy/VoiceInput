use crate::spike::error::ErrorClass;

#[derive(Clone, Copy, Debug)]
pub enum SpikePhase {
    CredentialsLoaded,
    Connecting,
    Listening,
    Finalizing,
    Completed,
    Failed,
}

impl SpikePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::CredentialsLoaded => "credentials_loaded",
            Self::Connecting => "connecting",
            Self::Listening => "listening",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

pub struct SafeEvent {
    pub phase: SpikePhase,
    pub elapsed_ms: u128,
    pub audio_frames: usize,
    pub error: Option<ErrorClass>,
}

impl SafeEvent {
    pub fn write_to_stderr(&self) {
        let error = self.error.map(ErrorClass::as_str).unwrap_or("none");
        eprintln!(
            "[spike] phase={} elapsed_ms={} audio_frames={} error={}",
            self.phase.as_str(),
            self.elapsed_ms,
            self.audio_frames,
            error
        );
    }
}
