//! Single-session dictation controller.
//!
//! It owns recording lifecycle, bounded final-result waiting, cancellation,
//! and result delivery. Status events deliberately exclude transcript text.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::insertion::{ForegroundTarget, InsertOutcome, TextInserter};
use crate::spike::{
    connect, parse_server_frame, Credentials, ErrorClass, RecognitionEvent, Recorder,
    ServerMessage, SpikeError,
};

const AUDIO_QUEUE_CAPACITY: usize = 64;
const FINAL_TIMEOUT: Duration = Duration::from_secs(12);
const FINALIZE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const RECORDING_LIMIT: Duration = Duration::from_secs(120);
const TERMINAL_STATUS_DELAY: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Idle,
    Starting,
    Listening,
    Finalizing,
    Inserted,
    Copied,
    PartialCopied,
    Error,
    Cancelled,
}

impl SessionPhase {
    pub fn presents_overlay(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionError {
    Credentials,
    RateLimit,
    Network,
    Microphone,
    AudioBackpressure,
    Protocol,
    NoFinal,
    FinalTimeout,
    Insertion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionStatus {
    pub phase: SessionPhase,
    pub elapsed_ms: u128,
    pub audio_frames: usize,
    pub error: Option<SessionError>,
}

impl SessionStatus {
    fn idle() -> Self {
        Self {
            phase: SessionPhase::Idle,
            elapsed_ms: 0,
            audio_frames: 0,
            error: None,
        }
    }

    fn running(phase: SessionPhase, started: Instant, audio_frames: usize) -> Self {
        Self {
            phase,
            elapsed_ms: started.elapsed().as_millis(),
            audio_frames,
            error: None,
        }
    }
}

#[derive(Clone)]
pub struct SessionController {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<ControllerState>,
    status_tx: watch::Sender<SessionStatus>,
    inserter: Arc<dyn TextInserter>,
}

struct ControllerState {
    status: SessionStatus,
    next_session_id: u64,
    active: Option<ActiveSession>,
}

#[derive(Clone)]
struct ActiveSession {
    id: u64,
    stop: CancellationToken,
    cancel: CancellationToken,
}

enum ToggleAction {
    Start {
        control: ActiveSession,
        target: ForegroundTarget,
    },
    Stop,
    Cancel,
    None,
}

enum RunOutcome {
    Final(String),
    Partial(String),
    Cancelled,
    Failed(SpikeError),
}

impl SessionController {
    pub fn new(inserter: Arc<dyn TextInserter>) -> Self {
        let initial = SessionStatus::idle();
        let (status_tx, _) = watch::channel(initial.clone());
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(ControllerState {
                    status: initial,
                    next_session_id: 0,
                    active: None,
                }),
                status_tx,
                inserter,
            }),
        }
    }

    pub fn status(&self) -> SessionStatus {
        self.inner
            .state
            .lock()
            .expect("session state lock poisoned")
            .status
            .clone()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<SessionStatus> {
        self.inner.status_tx.subscribe()
    }

    pub fn toggle(&self) {
        let action = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("session state lock poisoned");
            match state.status.phase {
                SessionPhase::Idle
                | SessionPhase::Inserted
                | SessionPhase::Copied
                | SessionPhase::PartialCopied
                | SessionPhase::Error
                | SessionPhase::Cancelled => {
                    state.next_session_id += 1;
                    let control = ActiveSession {
                        id: state.next_session_id,
                        stop: CancellationToken::new(),
                        cancel: CancellationToken::new(),
                    };
                    state.active = Some(control.clone());
                    state.status = SessionStatus {
                        phase: SessionPhase::Starting,
                        elapsed_ms: 0,
                        audio_frames: 0,
                        error: None,
                    };
                    ToggleAction::Start {
                        control,
                        target: self.inner.inserter.capture_target(),
                    }
                }
                SessionPhase::Listening => {
                    if let Some(active) = &state.active {
                        active.stop.cancel();
                        state.status.phase = SessionPhase::Finalizing;
                        ToggleAction::Stop
                    } else {
                        ToggleAction::None
                    }
                }
                SessionPhase::Starting | SessionPhase::Finalizing => {
                    if let Some(active) = &state.active {
                        active.cancel.cancel();
                        ToggleAction::Cancel
                    } else {
                        ToggleAction::None
                    }
                }
            }
        };

        match action {
            ToggleAction::Start { control, target } => {
                self.publish_current();
                let controller = self.clone();
                std::thread::Builder::new()
                    .name("voiceinput-session".to_owned())
                    .spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("VoiceInput session runtime could not start");
                        runtime.block_on(controller.run_session(control, target));
                    })
                    .expect("VoiceInput session thread could not start");
            }
            ToggleAction::Stop | ToggleAction::Cancel => self.publish_current(),
            ToggleAction::None => {}
        }
    }

    pub fn cancel(&self) {
        let cancelled = {
            let state = self
                .inner
                .state
                .lock()
                .expect("session state lock poisoned");
            state.active.as_ref().map(|active| active.cancel.clone())
        };
        if let Some(cancelled) = cancelled {
            cancelled.cancel();
        }
    }

    async fn run_session(&self, control: ActiveSession, target: ForegroundTarget) {
        let started = Instant::now();
        let outcome = run_recognition(self, &control, started).await;
        let status = match outcome {
            RunOutcome::Final(text) => match self.inner.inserter.insert_or_copy(target, &text) {
                InsertOutcome::Inserted => {
                    SessionStatus::running(SessionPhase::Inserted, started, 0)
                }
                InsertOutcome::Copied => SessionStatus::running(SessionPhase::Copied, started, 0),
                InsertOutcome::Failed => SessionStatus {
                    phase: SessionPhase::Error,
                    elapsed_ms: started.elapsed().as_millis(),
                    audio_frames: 0,
                    error: Some(SessionError::Insertion),
                },
            },
            RunOutcome::Partial(text) => match self.inner.inserter.copy_only(&text) {
                InsertOutcome::Copied | InsertOutcome::Inserted => {
                    SessionStatus::running(SessionPhase::PartialCopied, started, 0)
                }
                InsertOutcome::Failed => SessionStatus {
                    phase: SessionPhase::Error,
                    elapsed_ms: started.elapsed().as_millis(),
                    audio_frames: 0,
                    error: Some(SessionError::Insertion),
                },
            },
            RunOutcome::Cancelled => SessionStatus::running(SessionPhase::Cancelled, started, 0),
            RunOutcome::Failed(error) => SessionStatus {
                phase: SessionPhase::Error,
                elapsed_ms: started.elapsed().as_millis(),
                audio_frames: 0,
                error: Some(SessionError::from(error.class())),
            },
        };
        self.finish(control.id, status);
    }

    fn publish_for(&self, session_id: u64, status: SessionStatus) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("session state lock poisoned");
        if state.active.as_ref().map(|active| active.id) != Some(session_id) {
            return false;
        }
        state.status = status;
        drop(state);
        self.publish_current();
        true
    }

    fn finish(&self, session_id: u64, status: SessionStatus) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("session state lock poisoned");
            if state.active.as_ref().map(|active| active.id) != Some(session_id) {
                return;
            }
            state.active = None;
            state.status = status;
        }
        self.publish_current();

        let controller = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(TERMINAL_STATUS_DELAY);
            controller.reset_if_unchanged(session_id);
        });
    }

    fn reset_if_unchanged(&self, session_id: u64) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("session state lock poisoned");
        if state.active.is_none() && state.next_session_id == session_id {
            state.status = SessionStatus::idle();
            drop(state);
            self.publish_current();
        }
    }

    fn publish_current(&self) {
        let status = self.status();
        let _ = self.inner.status_tx.send(status);
    }
}

impl From<ErrorClass> for SessionError {
    fn from(value: ErrorClass) -> Self {
        match value {
            ErrorClass::Credentials => Self::Credentials,
            ErrorClass::RateLimit => Self::RateLimit,
            ErrorClass::Network => Self::Network,
            ErrorClass::Microphone => Self::Microphone,
            ErrorClass::AudioBackpressure => Self::AudioBackpressure,
            ErrorClass::Protocol => Self::Protocol,
            ErrorClass::NoFinal => Self::NoFinal,
            ErrorClass::FinalTimeout => Self::FinalTimeout,
        }
    }
}

async fn run_recognition(
    controller: &SessionController,
    control: &ActiveSession,
    started: Instant,
) -> RunOutcome {
    let credentials = match Credentials::load() {
        Ok(credentials) => credentials,
        Err(error) => return RunOutcome::Failed(error),
    };
    let (mut session, mut server_rx) = tokio::select! {
        _ = control.cancel.cancelled() => return RunOutcome::Cancelled,
        result = connect(credentials, &[]) => match result {
            Ok(connection) => connection,
            Err(error) => return RunOutcome::Failed(error),
        },
    };
    let (audio_tx, mut audio_rx) = mpsc::channel(AUDIO_QUEUE_CAPACITY);
    let recorder = match Recorder::start_default(audio_tx) {
        Ok(recorder) => recorder,
        Err(error) => return RunOutcome::Failed(error),
    };
    let mut recorder_failure = recorder.failure_receiver();
    if !controller.publish_for(
        control.id,
        SessionStatus::running(SessionPhase::Listening, started, 0),
    ) {
        let _ = recorder.stop();
        return RunOutcome::Cancelled;
    }

    let recording_limit = tokio::time::sleep(RECORDING_LIMIT);
    tokio::pin!(recording_limit);
    let mut partial = None;
    loop {
        tokio::select! {
            _ = control.cancel.cancelled() => {
                let _ = recorder.stop();
                return RunOutcome::Cancelled;
            }
            _ = control.stop.cancelled() => break,
            _ = &mut recording_limit => break,
            Some(pcm) = audio_rx.recv() => {
                if let Err(error) = session.send_audio(&pcm).await {
                    let _ = recorder.stop();
                    return with_partial(error, partial);
                }
            }
            message = server_rx.recv() => match receive_message(message, &mut partial) {
                Ok(Some(RecognitionEvent::Final(text))) => {
                    let _ = recorder.stop();
                    return RunOutcome::Final(text);
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = recorder.stop();
                    return with_partial(error, partial);
                }
            },
            changed = recorder_failure.changed() => {
                let _ = recorder.stop();
                if changed.is_err() {
                    return RunOutcome::Failed(SpikeError::MicrophoneFailed);
                }
                let error = (*recorder_failure.borrow())
                    .map(|failure| failure.into_error())
                    .unwrap_or(SpikeError::MicrophoneFailed);
                return RunOutcome::Failed(error);
            }
        }
    }

    if let Err(error) = recorder.stop() {
        return RunOutcome::Failed(error);
    }
    let finalize_deadline = Instant::now() + FINALIZE_DRAIN_TIMEOUT;
    while let Ok(pcm) = audio_rx.try_recv() {
        if let Err(error) = session.send_audio_until(&pcm, finalize_deadline).await {
            return with_partial(error, partial);
        }
    }
    if let Err(error) = session.send_last_frame_until(finalize_deadline).await {
        return with_partial(error, partial);
    }
    controller.publish_for(
        control.id,
        SessionStatus::running(SessionPhase::Finalizing, started, session.audio_frames),
    );

    match tokio::time::timeout(
        FINAL_TIMEOUT,
        await_final(&mut server_rx, &mut partial, &control.cancel),
    )
    .await
    {
        Ok(Ok(text)) if !text.trim().is_empty() => RunOutcome::Final(text),
        Ok(Ok(_)) => with_partial(SpikeError::NoFinalResult, partial),
        Ok(Err(outcome)) => outcome,
        Err(_) => with_partial(SpikeError::FinalResultTimeout, partial),
    }
}

fn receive_message(
    message: Option<ServerMessage>,
    partial: &mut Option<String>,
) -> Result<Option<RecognitionEvent>, SpikeError> {
    match message {
        Some(ServerMessage::Frame(bytes)) => match parse_server_frame(&bytes)? {
            Some(RecognitionEvent::Partial(text)) => {
                *partial = Some(text);
                Ok(None)
            }
            event => Ok(event),
        },
        Some(ServerMessage::Closed) | None => Err(SpikeError::NoFinalResult),
        Some(ServerMessage::NetworkFailed) => Err(SpikeError::Network),
    }
}

async fn await_final(
    server_rx: &mut mpsc::Receiver<ServerMessage>,
    partial: &mut Option<String>,
    cancel: &CancellationToken,
) -> Result<String, RunOutcome> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Err(RunOutcome::Cancelled),
            message = server_rx.recv() => match receive_message(message, partial) {
                Ok(Some(RecognitionEvent::Final(text))) => return Ok(text),
                Ok(_) => {}
                Err(error) => return Err(with_partial(error, partial.take())),
            }
        }
    }
}

fn with_partial(error: SpikeError, partial: Option<String>) -> RunOutcome {
    if matches!(
        error,
        SpikeError::Network | SpikeError::NoFinalResult | SpikeError::FinalResultTimeout
    ) {
        if let Some(partial) = partial.filter(|text| !text.trim().is_empty()) {
            return RunOutcome::Partial(partial);
        }
    }
    RunOutcome::Failed(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_text_is_used_only_for_recoverable_finalization_failures() {
        assert!(matches!(
            with_partial(SpikeError::Network, Some("partial".to_owned())),
            RunOutcome::Partial(_)
        ));
        assert!(matches!(
            with_partial(SpikeError::Protocol, Some("partial".to_owned())),
            RunOutcome::Failed(SpikeError::Protocol)
        ));
    }

    #[test]
    fn error_classes_remain_user_safe() {
        assert_eq!(
            SessionError::from(ErrorClass::Credentials),
            SessionError::Credentials
        );
        assert_eq!(
            SessionError::from(ErrorClass::Protocol),
            SessionError::Protocol
        );
    }

    #[test]
    fn terminal_status_returns_to_idle_after_its_visible_delay() {
        let controller = SessionController::new(Arc::new(crate::insertion::WindowsTextInserter));
        let control = ActiveSession {
            id: 1,
            stop: CancellationToken::new(),
            cancel: CancellationToken::new(),
        };
        {
            let mut state = controller.inner.state.lock().expect("session state lock");
            state.next_session_id = control.id;
            state.active = Some(control);
        }

        controller.finish(
            1,
            SessionStatus::running(SessionPhase::Cancelled, Instant::now(), 0),
        );
        assert_eq!(controller.status().phase, SessionPhase::Cancelled);

        std::thread::sleep(TERMINAL_STATUS_DELAY + Duration::from_millis(100));
        assert_eq!(controller.status().phase, SessionPhase::Idle);
    }

    #[test]
    fn overlay_is_hidden_only_while_idle() {
        assert!(!SessionPhase::Idle.presents_overlay());
        for phase in [
            SessionPhase::Starting,
            SessionPhase::Listening,
            SessionPhase::Finalizing,
            SessionPhase::Inserted,
            SessionPhase::Copied,
            SessionPhase::PartialCopied,
            SessionPhase::Error,
            SessionPhase::Cancelled,
        ] {
            assert!(phase.presents_overlay(), "{phase:?} must be visible");
        }
    }

    #[test]
    fn second_toggle_from_listening_publishes_finalizing() {
        let controller = SessionController::new(Arc::new(crate::insertion::WindowsTextInserter));
        let stop = CancellationToken::new();
        {
            let mut state = controller.inner.state.lock().expect("session state lock");
            state.next_session_id = 1;
            state.active = Some(ActiveSession {
                id: 1,
                stop: stop.clone(),
                cancel: CancellationToken::new(),
            });
            state.status = SessionStatus::running(SessionPhase::Listening, Instant::now(), 0);
        }

        controller.toggle();

        assert!(stop.is_cancelled());
        assert_eq!(controller.status().phase, SessionPhase::Finalizing);
    }
}
