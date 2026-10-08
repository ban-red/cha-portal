// @cha/player: watch and drive an environment in the browser (plan §6.1).
// Framework-agnostic: give it a <video> element and a signalling function.
export { supportsPyroWave } from "./pyro";
export {
  FRAME_RATES,
  Player,
  isPyroWave,
  supportedCodecs,
  supportsWebTransport,
  type Codec,
  type FrameRate,
  type PlayerOptions,
  type PlayerState,
  type SetupStatus,
  type Transport,
  type WebTransportOffer,
} from "./player";
export { OVERLAY_LEVELS, type OverlayLevel, type OverlayState } from "./overlay";
export { CaptureMode, type CaptureState, type CaptureView } from "./captureMode";
export type { InputMode } from "./inputMode";
export { handable, parseWatchers, watcherLabel, type Watcher } from "./watchers";
export {
  assessHealth,
  HEALTH_WINDOW,
  type HealthAssessment,
  type HealthContext,
  type HealthGrade,
  type HealthIssue,
  type IssueSeverity,
} from "./health";
export {
  FREEZE_MS,
  recordingMarkdown,
  summarizeRecording,
  type RecordingMeta,
  type RecordingSummary,
  type Spread,
} from "./recording";
export type { NodeStats, StatsSnapshot } from "./stats";
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
