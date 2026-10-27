<script lang="ts">
  import { loadAttachment, type Loaded } from "../lib/attachments";
  import { size } from "../lib/format";
  import type { AttachmentView } from "../lib/types";

  let { memoId, attachment, onclose }: { memoId: string; attachment: AttachmentView; onclose: () => void } =
    $props();
  let loaded = $state<Loaded | undefined>(undefined);
  let failed = $state(false);
  let dialog: HTMLDialogElement;

  $effect(() => {
    dialog.showModal();
    loadAttachment(memoId, attachment, "original").then(
      (result) => (loaded = result),
      () => (failed = true),
    );
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
<dialog bind:this={dialog} {onclose} onclick={() => dialog.close()}>
  {#if failed}
    <p class="message">The image could not be loaded.</p>
  {:else if loaded === undefined}
    <p class="message">Loading…</p>
  {:else if loaded === null}
    <p class="message">This image has not been uploaded yet.</p>
  {:else if loaded.renderable}
    <img src={loaded.url} alt="" width={attachment.width} height={attachment.height} />
  {:else}
    <p class="message">
      The decrypted file is not a valid {attachment.mime} image, so it is not shown.
      <a href={loaded.url} download="attachment.bin">Download it ({size(attachment.size)})</a>
    </p>
  {/if}
</dialog>

<style>
  dialog {
    width: auto;
    max-width: calc(100vw - 24px);
    max-height: calc(100vh - 24px);
    padding: 0;
    background: transparent;
    border: none;
    box-shadow: none;
  }
  img {
    display: block;
    max-width: calc(100vw - 24px);
    max-height: calc(100vh - 24px);
    width: auto;
    height: auto;
    border-radius: var(--radius);
  }
  .message {
    background: var(--surface);
    padding: 20px;
    border-radius: var(--radius);
    margin: 0;
  }
</style>
