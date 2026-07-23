use std::io;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use voiceinput_lib::spike::{
    connect, parse_server_frame, Credentials, RecognitionEvent, Recorder, SafeEvent, ServerMessage,
    SpikeError, SpikePhase,
};

const FINAL_TIMEOUT: Duration = Duration::from_secs(12);
const AUDIO_QUEUE_CAPACITY: usize = 64;
const FINALIZE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        SafeEvent {
            phase: SpikePhase::Failed,
            elapsed_ms: 0,
            audio_frames: 0,
            error: Some(error.class()),
        }
        .write_to_stderr();
        eprintln!("失败：{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), SpikeError> {
    let hotwords = parse_hotwords()?;
    let started = Instant::now();
    let credentials = Credentials::load()?;
    SafeEvent {
        phase: SpikePhase::CredentialsLoaded,
        elapsed_ms: started.elapsed().as_millis(),
        audio_frames: 0,
        error: None,
    }
    .write_to_stderr();

    println!("已读取本地凭据。按 Enter 开始录音。");
    wait_for_enter().await?;

    SafeEvent {
        phase: SpikePhase::Connecting,
        elapsed_ms: started.elapsed().as_millis(),
        audio_frames: 0,
        error: None,
    }
    .write_to_stderr();
    let (mut session, mut server_rx) = connect(credentials, &hotwords).await?;
    let (audio_tx, mut audio_rx) = mpsc::channel(AUDIO_QUEUE_CAPACITY);
    let recorder = Recorder::start_default(audio_tx)?;
    let mut recorder_failure = recorder.failure_receiver();
    SafeEvent {
        phase: SpikePhase::Listening,
        elapsed_ms: started.elapsed().as_millis(),
        audio_frames: 0,
        error: None,
    }
    .write_to_stderr();
    println!("正在录音。说完后按 Enter 结束。");

    let mut stop = Box::pin(wait_for_enter());
    let mut partial = None;
    loop {
        tokio::select! {
            result = &mut stop => {
                result?;
                break;
            }
            Some(pcm) = audio_rx.recv() => session.send_audio(&pcm).await?,
            message = server_rx.recv() => receive_during_recording(message, &mut partial)?,
            changed = recorder_failure.changed() => {
                changed.map_err(|_| SpikeError::MicrophoneFailed)?;
                let failure = (*recorder_failure.borrow()).ok_or(SpikeError::MicrophoneFailed)?;
                return Err(failure.into_error());
            }
        }
    }

    recorder.stop()?;
    let finalize_deadline = Instant::now() + FINALIZE_DRAIN_TIMEOUT;
    while let Ok(pcm) = audio_rx.try_recv() {
        session.send_audio_until(&pcm, finalize_deadline).await?;
    }
    session.send_last_frame_until(finalize_deadline).await?;
    SafeEvent {
        phase: SpikePhase::Finalizing,
        elapsed_ms: started.elapsed().as_millis(),
        audio_frames: session.audio_frames,
        error: None,
    }
    .write_to_stderr();

    let final_text = tokio::time::timeout(FINAL_TIMEOUT, await_final(&mut server_rx, &mut partial))
        .await
        .map_err(|_| SpikeError::FinalResultTimeout)??;
    if final_text.trim().is_empty() {
        return Err(SpikeError::NoFinalResult);
    }

    SafeEvent {
        phase: SpikePhase::Completed,
        elapsed_ms: started.elapsed().as_millis(),
        audio_frames: session.audio_frames,
        error: None,
    }
    .write_to_stderr();
    println!("最终文本（仅本次终端显示，不写入日志）：\n{final_text}");
    Ok(())
}

fn receive_during_recording(
    message: Option<ServerMessage>,
    partial: &mut Option<String>,
) -> Result<(), SpikeError> {
    match message {
        Some(ServerMessage::Frame(bytes)) => match parse_server_frame(&bytes)? {
            Some(RecognitionEvent::Partial(text)) => *partial = Some(text),
            Some(RecognitionEvent::Final(_)) => return Err(SpikeError::Protocol),
            None => {}
        },
        Some(ServerMessage::Closed) | None => return Err(SpikeError::NoFinalResult),
        Some(ServerMessage::NetworkFailed) => return Err(SpikeError::Network),
    }
    Ok(())
}

async fn await_final(
    server_rx: &mut mpsc::Receiver<ServerMessage>,
    partial: &mut Option<String>,
) -> Result<String, SpikeError> {
    while let Some(message) = server_rx.recv().await {
        match message {
            ServerMessage::Frame(bytes) => match parse_server_frame(&bytes)? {
                Some(RecognitionEvent::Final(text)) => return Ok(text),
                Some(RecognitionEvent::Partial(text)) => *partial = Some(text),
                None => {}
            },
            ServerMessage::Closed => return Err(SpikeError::NoFinalResult),
            ServerMessage::NetworkFailed => return Err(SpikeError::Network),
        }
    }
    Err(SpikeError::NoFinalResult)
}

async fn wait_for_enter() -> Result<(), SpikeError> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|_| SpikeError::MicrophoneFailed)
            .map(|_| ())
    })
    .await
    .map_err(|_| SpikeError::MicrophoneFailed)?
}

fn parse_hotwords() -> Result<Vec<String>, SpikeError> {
    let mut args = std::env::args().skip(1);
    let mut hotwords = Vec::new();
    while let Some(argument) = args.next() {
        if argument != "--hotword" {
            return Err(SpikeError::InvalidArguments);
        }
        let word = args.next().ok_or(SpikeError::InvalidArguments)?;
        hotwords.push(word);
    }
    Ok(hotwords)
}
