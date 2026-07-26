import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
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
  starting: "正在启动听写",
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
  const [receivedAt, setReceivedAt] = useState(Date.now());
  const [now, setNow] = useState(Date.now());

  const updateStatus = (next: SessionStatus) => {
    setStatus(next);
    const receivedAt = Date.now();
    setReceivedAt(receivedAt);
    setNow(receivedAt);
  };

  useEffect(() => {
    let mounted = true;
    let unlisten: (() => void) | undefined;

    void (async () => {
      const dispose = await listen<SessionStatus>("voiceinput://status", (event) => {
        if (mounted) updateStatus(event.payload);
      });
      if (!mounted) {
        dispose();
        return;
      }
      unlisten = dispose;
      const next = await invoke<SessionStatus>("session_status");
      if (mounted) updateStatus(next);
    })();

    return () => {
      mounted = false;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (status.phase !== "listening") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [status.phase]);

  const elapsedMs =
    status.phase === "listening" ? status.elapsed_ms + now - receivedAt : status.elapsed_ms;
  const elapsed = new Date(Math.max(elapsedMs, 0)).toISOString().slice(14, 19);
  const hint =
    status.phase === "listening"
      ? "Alt + Space 结束  ·  Esc 取消"
      : ["starting", "finalizing"].includes(status.phase)
        ? "Esc 取消"
        : null;

  return (
    <main aria-label="语音输入状态">
      <section className={`capsule capsule-${status.phase}`} aria-live="polite">
        <div className="status-row">
          <span className={`status-dot status-${status.phase}`} aria-hidden="true" />
          <p className="status">{labels[status.phase]}</p>
          {status.phase === "listening" && <time>{elapsed}</time>}
        </div>

        {hint && <p className="hint">{hint}</p>}
        {status.error && <p className="error">{errorHints[status.error]}</p>}
      </section>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
