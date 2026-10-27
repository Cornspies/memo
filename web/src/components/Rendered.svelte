<script lang="ts">
  import { tick } from "svelte";
  import { loadAttachment } from "../lib/attachments";
  import { backend } from "../lib/state.svelte";
  import type { AttachmentView } from "../lib/types";

  let {
    memoId,
    body,
    attachments,
    writable = false,
    ontask,
    ontag,
    onimage,
  }: {
    memoId: string;
    body: string;
    attachments: AttachmentView[];
    writable?: boolean;
    ontask?: (offset: number) => void;
    ontag?: (tag: string) => void;
    onimage?: (attachment: AttachmentView) => void;
  } = $props();

  let html = $state("");
  let container: HTMLDivElement;

  $effect(() => {
    const source = body;
    backend.render(source).then((rendered) => {
      if (source === body) html = rendered;
    });
  });

  $effect(() => {
    void html;
    void attachments;
    void writable;
    tick().then(hydrate);
  });

  /** Fills in what the renderer leaves to the client: image sources and task boxes. */
  function hydrate() {
    for (const img of container.querySelectorAll<HTMLImageElement>("img[data-attachment]")) {
      const attachment = attachments.find((a) => a.id === img.dataset.attachment);
      if (!attachment) {
        img.alt = img.alt || "Missing image";
        continue;
      }
      const shown = attachment.thumb ?? attachment;
      img.width = shown.width;
      img.height = shown.height;
      const kind = attachment.thumb ? "thumbnail" : "original";
      if (!(kind === "thumbnail" ? attachment.thumb_available : attachment.available)) {
        img.alt = "Uploading…";
        continue;
      }
      loadAttachment(memoId, attachment, kind).then(
        (loaded) => {
          if (!loaded) img.alt = "Uploading…";
          else if (loaded.renderable) img.src = loaded.url;
          else img.alt = "This file is not a valid image. Open it to download.";
        },
        () => (img.alt = "Image unavailable"),
      );
    }
    for (const box of container.querySelectorAll<HTMLInputElement>("input[data-task]")) {
      box.disabled = !writable || !ontask;
    }
  }

  function click(event: MouseEvent) {
    const target = event.target as HTMLElement;
    if (target instanceof HTMLInputElement && target.dataset.task) {
      event.preventDefault();
      ontask?.(Number(target.dataset.task));
    } else if (target.classList.contains("tag") && target.dataset.tag) {
      ontag?.(target.dataset.tag);
    } else if (target instanceof HTMLImageElement && target.dataset.attachment) {
      const attachment = attachments.find((a) => a.id === target.dataset.attachment);
      if (attachment) onimage?.(attachment);
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="markdown" bind:this={container} onclick={click} role="presentation">
  <!-- Safe: memo_core::render escapes raw HTML and allows only safe links. -->
  {@html html}
</div>

<style>
  .markdown :global(img.attachment) {
    max-width: min(100%, 512px);
  }
</style>
