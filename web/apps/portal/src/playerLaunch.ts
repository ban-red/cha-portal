import { ApiError, api } from "./api";
import { playerLink } from "./player";

/**
 * Asks the portal for a ticket and hands it to Cha Player through the `cha://`
 * link (the browser asks the user before opening the app). `launch` is an app
 * template id to start once signed in. Returns an error message, or null.
 */
export async function openInPlayer(launch?: string): Promise<string | null> {
  try {
    const { ticket } = await api.deviceTicket(launch);
    window.location.href = playerLink(window.location.origin, ticket, launch);
    return null;
  } catch (err) {
    return err instanceof ApiError && err.message ? err.message : "Couldn't open Cha Player.";
  }
}
