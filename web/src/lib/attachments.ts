// Decrypted images as `blob:` URLs with the type from the payload, cached for the page's lifetime.
// Bytes that do not match their stated type get a download URL instead (`docs/crypto.md#attachments`).

import { backend } from "./state.svelte";
import type { AttachmentKind, AttachmentView } from "./types";

export type Loaded = { url: string; renderable: boolean } | null;

const cache = new Map<string, Promise<Loaded>>();

export function loadAttachment(memoId: string, attachment: AttachmentView, kind: AttachmentKind): Promise<Loaded> {
  const key = `${attachment.id}/${kind}`;
  let loaded = cache.get(key);
  if (!loaded) {
    const mime = kind === "thumbnail" && attachment.thumb ? attachment.thumb.mime : attachment.mime;
    loaded = backend.attachment(memoId, attachment.id, kind).then(
      (result) =>
        result && {
          url: URL.createObjectURL(
            new Blob([result.bytes as BlobPart], { type: result.renderable ? mime : "application/octet-stream" }),
          ),
          renderable: result.renderable,
        },
    );
    cache.set(key, loaded);
    // Not uploaded yet, or failed: ask again next time.
    loaded.then(
      (result) => result || cache.delete(key),
      () => cache.delete(key),
    );
  }
  return loaded;
}
