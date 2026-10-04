// WebRTC DataChannel receiver (main thread: RTCPeerConnection is not
// available in workers). `media` is unordered with maxRetransmits 0, the
// closest DataChannel equivalent of QUIC datagrams.

import { trafficQuery, type TrafficConfig } from "./proto";
import { Run, type RunProgress, type RunResult } from "./run";

export interface WebRtcStart {
  host: string;
  httpPort: number;
  config: TrafficConfig;
}

export async function runWebRtc(
  start: WebRtcStart,
  onProgress: (p: RunProgress) => void,
): Promise<RunResult> {
  const pc = new RTCPeerConnection();
  const control = pc.createDataChannel("control");
  const media = pc.createDataChannel("media", { ordered: false, maxRetransmits: 0 });
  media.binaryType = "arraybuffer";

  const run = new Run("webrtc", start.config, (line) => {
    if (control.readyState === "open") control.send(line);
  }, onProgress);

  control.onmessage = (e: MessageEvent<string | ArrayBuffer>) =>
    run.onControl(typeof e.data === "string" ? e.data : new Uint8Array(e.data));
  media.onmessage = (e: MessageEvent<ArrayBuffer>) => run.onDatagram(new Uint8Array(e.data));
  pc.onconnectionstatechange = () => {
    if (pc.connectionState === "failed" || pc.connectionState === "closed") {
      run.finish(`connection ${pc.connectionState}`);
    }
  };

  const opened = Promise.all([waitOpen(control), waitOpen(media)]);
  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);
  const base = `http://${start.host.includes(":") ? `[${start.host}]` : start.host}:${start.httpPort}`;
  const res = await fetch(
    `${base}/webrtc/offer?${trafficQuery(start.config)}&host=${encodeURIComponent(start.host)}`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(pc.localDescription),
    },
  );
  if (!res.ok) throw new Error(`offer rejected: ${res.status} ${await res.text()}`);
  await pc.setRemoteDescription((await res.json()) as RTCSessionDescriptionInit);
  await opened;

  run.start();
  const result = await run.result;
  pc.close();
  return result;
}

function waitOpen(channel: RTCDataChannel): Promise<void> {
  return new Promise((resolve, reject) => {
    channel.onopen = () => resolve();
    channel.onerror = () => reject(new Error(`channel ${channel.label} failed`));
    setTimeout(() => reject(new Error(`channel ${channel.label} did not open`)), 10_000);
  });
}
