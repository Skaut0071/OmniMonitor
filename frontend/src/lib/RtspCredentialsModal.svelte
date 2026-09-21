<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import { getRtspCredentials, type RtspCredentials } from "./api";

  const dispatch = createEventDispatcher();

  let creds: RtspCredentials | null = null;
  let error = "";
  let copied = "";

  // Plain strings (not the nullable `creds` itself) so the markup below
  // - which only renders once `creds` is truthy anyway - doesn't have to
  // fight TypeScript's null-narrowing across template expressions.
  $: username = creds?.username ?? "";
  $: password = creds?.password ?? "";

  onMount(async () => {
    try {
      creds = await getRtspCredentials();
    } catch (e) {
      error = (e as Error).message;
    }
  });

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") dispatch("close");
  }

  function exampleUrl(cameraId: string): string {
    if (!creds) return "";
    return `rtsp://${creds.username}:${creds.password}@${window.location.hostname}:${creds.port}/${cameraId}`;
  }

  async function copy(text: string, label: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = label;
      setTimeout(() => (copied = ""), 1500);
    } catch {
      // clipboard permission denied - not worth surfacing an error for
    }
  }
</script>

<svelte:window on:keydown={onKeydown} />

<div class="backdrop" role="presentation" on:click={() => dispatch("close")}>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="RTSP credentials"
    on:click|stopPropagation
  >
    <h2>RTSP credentials</h2>
    <p class="hint">
      Every camera is also reachable by any RTSP client (VLC, ffplay, another
      NVR) at <code>rtsp://&lt;host&gt;:{creds?.port ?? 5544}/&lt;camera-id&gt;</code>.
      These credentials are required to connect.
    </p>

    {#if error}
      <p class="error">{error}</p>
    {:else if !creds}
      <p class="hint">Loading…</p>
    {:else}
      <label>
        Username
        <div class="row">
          <input readonly value={username} />
          <button class="ghost" on:click={() => copy(username, "username")}>
            {copied === "username" ? "Copied" : "Copy"}
          </button>
        </div>
      </label>
      <label>
        Password
        <div class="row">
          <input readonly value={password} />
          <button class="ghost" on:click={() => copy(password, "password")}>
            {copied === "password" ? "Copied" : "Copy"}
          </button>
        </div>
      </label>
      <label>
        Example URL (replace &lt;camera-id&gt;)
        <div class="row">
          <input readonly value={exampleUrl("<camera-id>")} />
          <button class="ghost" on:click={() => copy(exampleUrl("<camera-id>"), "url")}>
            {copied === "url" ? "Copied" : "Copy"}
          </button>
        </div>
      </label>
    {/if}

    <div class="actions">
      <button class="primary" on:click={() => dispatch("close")}>Close</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
    padding: 1rem;
  }
  .modal {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.5rem;
    width: min(420px, 100%);
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
  h2 {
    margin: 0 0 0.25rem;
    font-size: 1.05rem;
  }
  .hint {
    font-size: 0.8rem;
    color: var(--text-dim);
    margin: 0 0 0.25rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.8rem;
    color: var(--text-dim);
  }
  .row {
    display: flex;
    gap: 0.4rem;
  }
  input {
    flex: 1;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.45rem 0.6rem;
    color: var(--text);
    font-size: 0.85rem;
    font-family: monospace;
  }
  .error {
    color: #e74c3c;
    font-size: 0.8rem;
    margin: 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.45rem 1rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .primary {
    background: var(--accent);
    color: #fff;
  }
  .ghost {
    background: transparent;
    color: var(--text-dim);
    border: 1px solid var(--border);
    white-space: nowrap;
  }
</style>
