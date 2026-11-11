<script lang="ts">
  import { backend, errorText } from "../lib/state.svelte";
  import Card from "./Card.svelte";

  let { onrecoverykey }: { onrecoverykey: (key: string) => void } = $props();

  type Mode = "login" | "register" | "recover";
  let mode = $state<Mode>(location.hash.startsWith("#/invite/") ? "register" : "login");
  let username = $state("");
  let password = $state("");
  let confirm = $state("");
  let invite = $state(location.hash.startsWith("#/invite/") ? decodeURIComponent(location.hash.slice(9)) : "");
  let recoveryKey = $state("");
  /** The desktop app asks which server; the browser uses the one it was loaded from. */
  let server = $state(readServer());

  function readServer() {
    try {
      return localStorage.getItem("memo-server") ?? "";
    } catch {
      return "";
    }
  }
  let busy = $state(false);
  let error = $state<string | null>(null);

  const titles: Record<Mode, string> = {
    login: "Sign in",
    register: "Create your account",
    recover: "Recover your account",
  };

  function switchTo(next: Mode) {
    mode = next;
    error = null;
    password = confirm = "";
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    if (mode !== "login") {
      const problem = await backend.passwordProblem(password);
      if (problem) return (error = problem);
      if (password !== confirm) return (error = "The passwords do not match.");
    }
    busy = true;
    const address = backend.native ? server : undefined;
    try {
      if (mode === "login") {
        await backend.login(username, password, address);
      } else if (mode === "register") {
        onrecoverykey(await backend.register(username, password, invite, address));
      } else {
        onrecoverykey(await backend.recover(username, recoveryKey, password, address));
      }
      try {
        if (address) localStorage.setItem("memo-server", address.trim());
      } catch {
        // Only a convenience.
      }
    } catch (e) {
      error = errorText(e, {
        conflict: "This username is taken.",
        bad_request: mode === "register" ? "Usernames are 3–32 characters: a–z, 0–9, _ . -" : "",
        invalid_credentials: mode === "recover" ? "Wrong username or recovery key." : "Wrong username or password.",
      });
    } finally {
      busy = false;
    }
  }
</script>

<Card title={titles[mode]}>
  <form onsubmit={submit}>
    {#if error}<p class="error">{error}</p>{/if}
    {#if backend.native}
      <label class="field">
        <span>Server</span>
        <input
          type="text"
          bind:value={server}
          required
          placeholder="memo.example.org"
          autocomplete="url"
          autocapitalize="none"
          spellcheck="false"
        />
      </label>
    {/if}
    {#if mode === "register"}
      <label class="field">
        <span>Invite code</span>
        <input type="text" bind:value={invite} required autocomplete="off" spellcheck="false" />
      </label>
    {/if}
    <label class="field">
      <span>Username</span>
      <input
        type="text"
        bind:value={username}
        required
        autocomplete="username"
        autocapitalize="none"
        spellcheck="false"
      />
    </label>
    {#if mode === "recover"}
      <label class="field">
        <span>Recovery key</span>
        <input type="text" class="mono" bind:value={recoveryKey} required autocomplete="off" spellcheck="false" />
      </label>
    {/if}
    <label class="field">
      <span>{mode === "login" ? "Password" : "New password"}</span>
      <input
        type="password"
        bind:value={password}
        required
        autocomplete={mode === "login" ? "current-password" : "new-password"}
      />
    </label>
    {#if mode !== "login"}
      <label class="field">
        <span>Repeat the password</span>
        <input type="password" bind:value={confirm} required autocomplete="new-password" />
      </label>
      <p class="muted small-text">
        At least 12 characters. Your password never leaves this device; it encrypts your memos, so the
        server cannot reset it.
      </p>
    {/if}
    <button class="primary wide" disabled={busy}>
      {#if busy}Working…{:else}{mode === "login" ? "Sign in" : mode === "register" ? "Create account" : "Recover"}{/if}
    </button>
  </form>
  <div class="links small-text">
    {#if mode !== "login"}<button class="ghost small" onclick={() => switchTo("login")}>Sign in</button>{/if}
    {#if mode !== "register"}
      <button class="ghost small" onclick={() => switchTo("register")}>I have an invite</button>
    {/if}
    {#if mode !== "recover"}
      <button class="ghost small" onclick={() => switchTo("recover")}>Forgot password</button>
    {/if}
  </div>
</Card>

<style>
  .wide {
    width: 100%;
    padding: 9px;
  }
  .links {
    display: flex;
    justify-content: center;
    gap: 4px;
    margin-top: 16px;
    flex-wrap: wrap;
  }
  .links button {
    color: var(--accent);
  }
</style>
