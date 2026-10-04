// Gamepads from the Gamepad API to the environment, on the control channel:
// each pad's buttons (0..1) and axes (-1..1), standard mapping, sent when they
// change. The streamer turns them into virtual Xbox 360 controllers. Pads
// show up after their first button press (the browser's rule).

type Send = (msg: Record<string, unknown>) => void;

/** Chrome samples pads at 250 Hz; polling at that rate adds no delay. */
const POLL_MS = 4;

export class GamepadCapture {
  private readonly last = new Map<number, string>();
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly onGone = (e: GamepadEvent) => {
    this.last.delete(e.gamepad.index);
    this.send({ k: "pad", i: e.gamepad.index, gone: true });
  };

  constructor(private readonly send: Send) {
    this.timer = setInterval(() => this.poll(), POLL_MS);
    window.addEventListener("gamepaddisconnected", this.onGone);
  }

  dispose(): void {
    clearInterval(this.timer);
    window.removeEventListener("gamepaddisconnected", this.onGone);
    this.releaseAll();
  }

  /** Leaves nothing held down remotely. */
  private releaseAll(): void {
    for (const i of this.last.keys()) this.send({ k: "pad", i, gone: true });
    this.last.clear();
  }

  private poll(): void {
    // Unfocused pages get no pad updates: let go rather than stick.
    if (!document.hasFocus()) {
      this.releaseAll();
      return;
    }
    for (const pad of navigator.getGamepads?.() ?? []) {
      if (!pad?.connected) continue;
      const b = pad.buttons.map((x) => Math.round(x.value * 1000) / 1000);
      const a = pad.axes.map((x) => Math.round(x * 1000) / 1000);
      const state = `${b.join(",")}|${a.join(",")}`;
      if (this.last.get(pad.index) === state) continue;
      this.last.set(pad.index, state);
      this.send({ k: "pad", i: pad.index, b, a });
    }
  }
}
