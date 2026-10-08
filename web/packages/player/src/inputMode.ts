// What a page may send up (ADR 0014, share links): everything, or only gamepads.

/**
 * `all`: keyboard, mouse, wheel, pointer lock, clipboard, resize and pads, as
 * far as the page has the controls. `pads`: only gamepads, whether or not the
 * page has the controls (a share link's guest never does); nothing else is
 * captured or sent.
 */
export type InputMode = "all" | "pads";

/** Whether the page listens to the keyboard, mouse and clipboard at all. */
export function capturesDevices(mode: InputMode): boolean {
  return mode === "all";
}

/** Whether the page sizes the picture, tells the cursor mode or copies text up. */
export function sendsControls(mode: InputMode): boolean {
  return mode === "all";
}

/** Whether `msg` (the body of an `input` message) goes up. */
export function inputAllowed(mode: InputMode, hasControl: boolean, msg: Record<string, unknown>): boolean {
  return mode === "pads" ? msg.k === "pad" : hasControl;
}
