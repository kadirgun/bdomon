import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

const DESIGN_W = 1920;
const DESIGN_H = 1080;
const HUD_W = 141;
const HUD_H = 52;
const PREVIEW_W = 480;
const SCALE = PREVIEW_W / DESIGN_W; // önizleme pikseli / tasarım pikseli

interface Status {
  running: boolean;
  rtssVersion: string;
  slot: number;
  fps: number;
  gpu: number;
  cpu: number;
  ping: number;
  x: number;
  y: number;
}

const clampPos = (x: number, y: number) => ({
  x: Math.round(Math.min(Math.max(x, 1), DESIGN_W - HUD_W - 1)),
  y: Math.round(Math.min(Math.max(y, 1), DESIGN_H - HUD_H - 1)),
});

function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [message, setMessage] = useState("");
  const [pos, setPos] = useState({ x: 1, y: 300 });
  const [input, setInput] = useState({ x: "1", y: "300" });
  const [dragging, setDragging] = useState(false);
  const previewRef = useRef<HTMLDivElement>(null);
  const grabOffset = useRef({ dx: 0, dy: 0 });
  const posRef = useRef(pos);
  posRef.current = pos;
  const posLoaded = useRef(false);

  const refresh = async () => {
    try {
      const s = await invoke<Status>("overlay_status");
      setStatus(s);
      if (!posLoaded.current) {
        posLoaded.current = true;
        const p = clampPos(s.x, s.y);
        setPos(p);
        setInput({ x: String(p.x), y: String(p.y) });
      }
    } catch (e) {
      setMessage(String(e));
    }
  };

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, 1000);
    return () => clearInterval(timer);
  }, []);

  const call = async (cmd: string) => {
    try {
      const msg = await invoke<string>(cmd);
      setMessage(msg);
      await refresh();
    } catch (e) {
      setMessage(String(e));
    }
  };

  // Konumu backend'e uygular (görüntü slotu anında taşınır).
  const applyPosition = async (x: number, y: number) => {
    const p = clampPos(x, y);
    setPos(p);
    setInput({ x: String(p.x), y: String(p.y) });
    try {
      const msg = await invoke<string>("overlay_set_position", { x: p.x, y: p.y });
      setMessage(msg);
    } catch (e) {
      setMessage(String(e));
    }
  };

  // Önizleme içinde sürükleme (pointer capture ile).
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const rect = previewRef.current!.getBoundingClientRect();
    grabOffset.current = {
      dx: e.clientX - rect.left - posRef.current.x * SCALE,
      dy: e.clientY - rect.top - posRef.current.y * SCALE,
    };
    setDragging(true);
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging || !previewRef.current) return;
    const rect = previewRef.current.getBoundingClientRect();
    const p = clampPos(
      (e.clientX - rect.left - grabOffset.current.dx) / SCALE,
      (e.clientY - rect.top - grabOffset.current.dy) / SCALE
    );
    setPos(p);
    setInput({ x: String(p.x), y: String(p.y) });
  };

  const onPointerUp = () => {
    if (!dragging) return;
    setDragging(false);
    applyPosition(posRef.current.x, posRef.current.y);
  };

  // x/y girişlerinden uygula (Enter veya düğme).
  const applyInput = () => {
    const x = Number(input.x) || 1;
    const y = Number(input.y) || 1;
    applyPosition(x, y);
  };

  return (
    <main className="panel">
      <h1>BDOMon HUD</h1>

      <div className="status">
        <span className={status?.running ? "dot on" : "dot"} />
        {status
          ? status.running
            ? `Running — RTSS ${status.rtssVersion}, slot ${status.slot}`
            : status.rtssVersion !== "not connected"
              ? `Paused — RTSS ${status.rtssVersion}`
              : "Not connected to RTSS (start RTSS)"
          : "Loading status..."}
      </div>

      <div className="row">
        <button onClick={() => call("overlay_start")}>Start</button>
        <button onClick={() => call("overlay_stop")}>Stop</button>
      </div>

      <div className="preview" ref={previewRef}>
        <div
          className={dragging ? "hudEl drag" : "hudEl"}
          style={{
            left: pos.x * SCALE,
            top: pos.y * SCALE,
            width: HUD_W * SCALE,
            height: HUD_H * SCALE,
          }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
        >
          <span className="mdot g" />
          <span className="mdot b" />
          <span className="mdot p" />
          <span className="mdot y" />
        </div>
      </div>
      <div className="hint">Drag the HUD element and drop it; the position is applied in-game instantly.</div>

      <div className="grid">
        <label>
          X
          <input
            type="number"
            value={input.x}
            onChange={(e) => setInput({ ...input, x: e.target.value })}
            onKeyDown={(e) => e.key === "Enter" && applyInput()}
            onBlur={applyInput}
          />
        </label>
        <label>
          Y
          <input
            type="number"
            value={input.y}
            onChange={(e) => setInput({ ...input, y: e.target.value })}
            onKeyDown={(e) => e.key === "Enter" && applyInput()}
            onBlur={applyInput}
          />
        </label>
      </div>

      <button onClick={applyInput}>Apply position</button>

      <button className="small" onClick={() => call("overlay_reload_profile")}>
        Reload RTSS profile
      </button>

      {status && (
        <div className="live">
          Live: FPS {status.fps.toFixed(0)} · GPU {status.gpu.toFixed(0)}% · CPU{" "}
          {status.cpu.toFixed(0)}% · {status.ping.toFixed(0)}ms
        </div>
      )}

      <div className="msg">{message}</div>
    </main>
  );
}

export default App;
