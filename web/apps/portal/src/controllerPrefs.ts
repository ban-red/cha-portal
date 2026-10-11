// Controller settings kept in this browser (localStorage, like the player's codec, transport and
// sound under `cha.player.*`). Not part of the in-stream UI spec: Cha Player has no WebHID.

/** Pass a wired Steam Controller through to Steam in the environment, raw (experimental). */
export const STEAM_PASSTHROUGH_KEY = "cha.player.steamPassthrough";

export function loadSteamPassthrough(): boolean {
  try {
    return localStorage.getItem(STEAM_PASSTHROUGH_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveSteamPassthrough(on: boolean): void {
  try {
    localStorage.setItem(STEAM_PASSTHROUGH_KEY, on ? "1" : "0");
  } catch {
    // Private window or blocked storage: it holds for this page only.
  }
}
