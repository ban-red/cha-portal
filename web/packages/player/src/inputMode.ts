// What a page may send up (ADRs 0014 and 0015, share links): everything, only gamepads, or nothing.

/**
 * `all`: keyboard, mouse, wheel, pointer lock, clipboard, resize and pads, as
 * far as the page has the controls. `pads`: only gamepads, whether or not the
 * page has the controls (a share link's guest never does); nothing else is
 * captured or sent. `none` (a viewer link's guest): nothing goes up, and no
 * device is read; the page only watches and listens.
 */
export type InputMode = "all" | "pads" | "none";

/** Whether the page listens to the keyboard, mouse and clipboard at all. */
export function capturesDevices(mode: InputMode): boolean {
  return mode === "all";
}

/** Whether the page reads gamepads (and plays their rumble). */
export function readsPads(mode: InputMode): boolean {
  return mode !== "none";
}

/** Whether the page sizes the picture, tells the cursor mode or copies text up. */
export function sendsControls(mode: InputMode): boolean {
  return mode === "all";
}

/** Whether `msg` (the body of an `input` message) goes up. */
export function inputAllowed(mode: InputMode, hasControl: boolean, msg: Record<string, unknown>): boolean {
  if (mode === "none") return false;
  return mode === "pads" ? msg.k === "pad" : hasControl;
}
