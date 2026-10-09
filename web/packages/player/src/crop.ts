// Some encoders pad the coded picture and put the real size only in a header
// that decoders don't treat as a crop: AMD's VA-API AV1 (Mesa radeonsi) codes
// the width up to a multiple of 64 and the height up to a multiple of 16, with
// the true size in `render_size`. H.264 and HEVC crop in the decoder.
//
// The streamer tells the page its true size (hello, and `resized`), so the page
// crops: WebCodecs frames get a visible rect, and a WebRTC `<video>` gets a CSS
// view box. Both are no-ops when the sizes match.

export interface Size {
  w: number;
  h: number;
}

/** Padding never reaches this many pixels; a bigger difference is a frame from before a resize, not padding. */
export const MAX_PAD = 64;

/**
 * The part of a `coded`-sized picture to show: `stream` when it is no bigger than
 * the coded size and the excess (right and bottom only) is below `MAX_PAD`; null
 * for the whole picture (sizes match, the size is unknown, or it doesn't fit).
 */
export function cropSize(coded: Size, stream: Size | null | undefined): Size | null {
  if (!stream) return null;
  const { w, h } = stream;
  if (!Number.isFinite(w) || !Number.isFinite(h) || w <= 0 || h <= 0) return null;
  if (w > coded.w || h > coded.h) return null;
  if (w === coded.w && h === coded.h) return null;
  if (coded.w - w >= MAX_PAD || coded.h - h >= MAX_PAD) return null;
  return { w, h };
}

/** A frame to show: `frame` itself, or a cropped view of it (which closes `frame`; no pixels are copied). */
export function cropFrame(frame: VideoFrame, stream: Size | null | undefined): VideoFrame {
  const crop = cropSize({ w: frame.codedWidth, h: frame.codedHeight }, stream);
  if (!crop) return frame;
  // The padding is outside the visible rect, which for a decoder's frame is the coded size.
  const vis = frame.visibleRect;
  if (vis && (vis.width !== frame.codedWidth || vis.height !== frame.codedHeight)) return frame;
  try {
    const cropped = new VideoFrame(frame, {
      visibleRect: { x: 0, y: 0, width: crop.w, height: crop.h },
      displayWidth: crop.w,
      displayHeight: crop.h,
    });
    frame.close();
    return cropped;
  } catch {
    return frame;
  }
}

/** CSS `object-view-box` showing the top-left `crop` of a `coded`-sized picture, or "" for all of it. */
export function viewBox(coded: Size, stream: Size | null | undefined): string {
  const crop = cropSize(coded, stream);
  if (!crop) return "";
  const right = ((coded.w - crop.w) / coded.w) * 100;
  const bottom = ((coded.h - crop.h) / coded.h) * 100;
  return `inset(0 ${right.toFixed(4)}% ${bottom.toFixed(4)}% 0)`;
}

/** The true size each element's picture is meant to show, as far as the page knows it. */
const streamSizes = new WeakMap<HTMLVideoElement, Size>();

export function setStreamSize(video: HTMLVideoElement, size: Size | null): void {
  if (size) streamSizes.set(video, size);
  else streamSizes.delete(video);
}

/**
 * The size of the picture on screen: the stream's true size when the element still shows
 * padding around it (WebRTC), else what the element decoded. Null before the first frame.
 */
export function pictureSize(video: HTMLVideoElement): Size | null {
  const coded = { w: video.videoWidth, h: video.videoHeight };
  if (!coded.w || !coded.h) return null;
  return cropSize(coded, streamSizes.get(video)) ?? coded;
}
