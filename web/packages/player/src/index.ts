// @cha/player: watch and drive an environment in the browser (plan §6.1).
// Framework-agnostic: give it a <video> element and a signalling function.
export { Player, supportedCodecs, type Codec, type PlayerOptions, type PlayerState } from "./player";
export type { StatsSnapshot } from "./stats";
export type { ProbeResult } from "./probe";
