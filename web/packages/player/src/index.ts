// @cha/player: watch and drive an environment in the browser (plan §6.1).
// Framework-agnostic: give it a <video> element and a signalling function.
export { supportsPyroWave } from "./pyro";
export {
  Player,
  isPyroWave,
  supportedCodecs,
  supportsWebTransport,
  type Codec,
  type PlayerOptions,
  type PlayerState,
  type SetupStatus,
  type Transport,
  type WebTransportOffer,
} from "./player";
export type { StatsSnapshot } from "./stats";
export type { ProbeResult } from "./probe";
export {
  ControllerManager,
  EXTRA,
  hidUnavailableReason,
  type BackendController,
  type BackendName,
  type Capabilities,
  type ControllerInfo,
  type ControllerState,
  type ControllerType,
  type ManagedController,
  type ManagerOptions,
  type RawReport,
  type Side,
  type TouchPoint,
} from "./controllers";
