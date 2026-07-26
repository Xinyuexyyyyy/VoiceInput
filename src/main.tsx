import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

type SessionPhase =
  | "idle"
  | "starting"
  | "listening"
  | "finalizing"
  | "inserted"
  | "copied"
  | "partial_copied"
  | "error"
  | "cancelled";

type SessionError =
  | "credentials"
  | "rate_limit"
  | "network"
  | "microphone"
  | "audio_backpressure"
  | "protocol"
  | "no_final"
  | "final_timeout"
  | "insertion";

type SessionStatus = {
  phase: SessionPhase;
  elapsed_ms: number;
  audio_frames: number;
  error: SessionError | null;
};

const labels: Record<SessionPhase, string> = {
  idle: "就绪",
  starting: "正在启动",
  listening: "正在聆听",
  finalizing: "正在生成最终文字",
  inserted: "已写入当前输入位置",
  copied: "未自动写入，已复制",
  partial_copied: "网络异常，已复制部分结果",
  error: "本次听写失败",
  cancelled: "已取消",
};

const errorHints: Record<SessionError, string> = {
  credentials: "请检查本地凭据和资源配置。",
  rate_limit: "当前请求受限，请稍后重试。",
  network: "网络连接失败，请检查网络后重试。",
  microphone: "麦克风不可用，请检查系统输入设备。",
  audio_backpressure: "音频采集过载，请重新开始本次听写。",
  protocol: "识别服务响应异常，请重新开始本次听写。",
  no_final: "未取得最终文字，请重新开始本次听写。",
  final_timeout: "等待最终文字超时，请重新开始本次听写。",
  insertion: "未能写入或复制文字，请重新开始本次听写。",
};

const initialStatus: SessionStatus = {
  phase: "idle",
  elapsed_ms: 0,
  audio_frames: 0,
  error: null,
};

function App() {
  const [status, setStatus] = useState<SessionStatus>(initialStatus);
  const active = useMemo(
    () => ["starting", "listening", "finalizing"].includes(status.phase),
    [status.phase],
  );

  useEffect(() => {
    let mounted = true;
    let unlisten: (() => void) | undefined;

    void (async () => {
      const dispose = await listen<SessionStatus>("voiceinput://status", (event) => {
        if (mounted) setStatus(event.payload);
      });
      if (!mounted) {
        dispose();
        return;
      }
      unlisten = dispose;
      const next = await invoke<SessionStatus>("session_status");
      if (mounted) setStatus(next);
    })();

    const refreshStatus = () => {
      void invoke<SessionStatus>("session_status").then((next) => {
        if (mounted) setStatus(next);
      });
    };
    const poll = window.setInterval(refreshStatus, 500);

    return () => {
      mounted = false;
      window.clearInterval(poll);
      unlisten?.();
    };
  }, []);

  const toggle = async () => {
    setStatus(await invoke<SessionStatus>("toggle_session"));
  };

  const cancel = async () => {
    setStatus(await invoke<SessionStatus>("cancel_session"));
  };

  const primaryLabel =
    status.phase === "listening"
      ? "停止录音"
      : active
        ? "取消本次"
        : "开始录音";

  return (
    <main aria-label="VoiceInput">
      <section className="panel" aria-live="polite">
        <div className="brand-row">
          <div>
            <p className="eyebrow">VOICE INPUT</p>
            <h1>VoiceInput</h1>
          </div>
          <span className={`status-dot status-${status.phase}`} aria-hidden="true" />
        </div>

        <p className="status">{labels[status.phase]}</p>
        <p className="shortcut">Ctrl + Alt + Space</p>

        <div className="actions">
          <button className="primary" type="button" onClick={() => void toggle()}>
            {primaryLabel}
          </button>
          <button type="button" onClick={() => void cancel()} disabled={!active}>
            取消
          </button>
        </div>

        {status.error && <p className="error">{errorHints[status.error]}</p>}
        <p className="privacy">不显示、不保存录音或转写正文。</p>
      </section>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
