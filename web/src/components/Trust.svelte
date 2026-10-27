<script lang="ts">
  import type { Trust } from "../lib/types";

  let { trust }: { trust: Trust } = $props();
  const labels: Record<Trust, [string, string]> = {
    me: ["", "You"],
    verified: ["accent", "Verified"],
    pinned: ["", "Not verified"],
    not_pinned: ["", "New contact"],
    changed: ["danger", "Key changed!"],
  };
</script>

<span class="badge {labels[trust][0]}">{labels[trust][1]}</span>
