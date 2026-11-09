// Picture settings kept in this browser (localStorage, like the controller settings): they belong to
// the screen a stream is watched on, not to an account. Not part of the in-stream UI spec; Cha
// Player has its own in its settings window.

/** Stretch PyroWave game pictures with SGSR 1 instead of the browser's own scaling (experimental). */
export const UPSCALER_KEY = "cha.player.upscaler";

export function loadUpscaler(): "sgsr" | undefined {
  try {
    return localStorage.getItem(UPSCALER_KEY) === "sgsr" ? "sgsr" : undefined;
  } catch {
    return undefined;
  }
}

export function saveUpscaler(on: boolean): void {
  try {
    if (on) localStorage.setItem(UPSCALER_KEY, "sgsr");
    else localStorage.removeItem(UPSCALER_KEY);
  } catch {
    // Private window or blocked storage: it holds for this page only.
  }
}
