// Images before encryption (`docs/crypto.md#attachments`), in the worker: memo-core checks the
// type and strips metadata; the canvas applies EXIF rotation, downscales, and makes thumbnails.

import { clean_image } from "../wasm/memo.js";

const THUMB_SIDE = 512;
/** Below this, the original doubles as its own thumbnail. */
const THUMB_MIN_BYTES = 256 * 1024;

export interface Encoded {
  mime: string;
  width: number;
  height: number;
  bytes: Uint8Array;
}

export interface Prepared extends Encoded {
  thumb: Encoded | null;
}

function cleaned(bytes: Uint8Array): Encoded & { orientation: number } {
  const image = clean_image(bytes);
  try {
    return {
      mime: image.mime(),
      width: image.width(),
      height: image.height(),
      orientation: image.orientation(),
      bytes: image.bytes(),
    };
  } finally {
    image.free();
  }
}

async function encode(bitmap: ImageBitmap, width: number, height: number, type: string, quality: number) {
  const canvas = new OffscreenCanvas(width, height);
  const context = canvas.getContext("2d");
  if (!context) throw new Error("no 2D canvas");
  if (type === "image/jpeg") {
    context.fillStyle = "#fff";
    context.fillRect(0, 0, width, height);
  }
  context.imageSmoothingQuality = "high";
  context.drawImage(bitmap, 0, 0, width, height);
  const blob = await canvas.convertToBlob({ type, quality });
  return { type: blob.type, bytes: new Uint8Array(await blob.arrayBuffer()) };
}

function fit(width: number, height: number, side: number): [number, number] {
  const scale = Math.min(1, side / Math.max(width, height));
  return [Math.max(1, Math.round(width * scale)), Math.max(1, Math.round(height * scale))];
}

/** `maxSide` downscales larger images (`null`: keep the size). GIFs are kept as they are. */
export async function prepareImage(file: Blob, maxSide: number | null): Promise<Prepared> {
  const input = new Uint8Array(await file.arrayBuffer());
  const original = cleaned(input);
  let image: Encoded = original;
  let bitmap: ImageBitmap | null = null;
  const decode = async () =>
    (bitmap ??= await createImageBitmap(new Blob([input as BlobPart], { type: original.mime }), {
      imageOrientation: "from-image",
    }));
  try {
    const tooLarge = maxSide !== null && Math.max(original.width, original.height) > maxSide;
    if (original.mime !== "image/gif" && (original.orientation !== 1 || tooLarge)) {
      // Re-encoding applies the rotation and drops all metadata.
      const decoded = await decode();
      const [width, height] = fit(decoded.width, decoded.height, maxSide ?? Infinity);
      const type = original.mime === "image/jpeg" ? "image/jpeg" : original.mime;
      const out = await encode(decoded, width, height, type, 0.9);
      image = cleaned(out.bytes);
    }
    let thumb: Encoded | null = null;
    if (Math.max(image.width, image.height) > THUMB_SIDE || image.bytes.length > THUMB_MIN_BYTES) {
      const decoded = await decode();
      const [width, height] = fit(decoded.width, decoded.height, THUMB_SIDE);
      let out = await encode(decoded, width, height, "image/webp", 0.8);
      // Browsers that cannot encode WebP fall back to PNG; JPEG is smaller.
      if (out.type !== "image/webp") out = await encode(decoded, width, height, "image/jpeg", 0.8);
      thumb = cleaned(out.bytes);
    }
    return { ...image, thumb };
  } finally {
    (bitmap as ImageBitmap | null)?.close();
  }
}
