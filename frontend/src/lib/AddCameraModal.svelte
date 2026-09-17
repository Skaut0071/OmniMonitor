<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import init, { validate_camera_name } from "./wasm/omni_wasm.js";
  import { createRtspCamera } from "./api";

  const dispatch = createEventDispatcher();

  let name = "";
  let url = "";
  let error = "";
  let wasmReady = false;
  let submitting = false;

  onMount(async () => {
    // Reuses the exact same validation rules the server enforces
    // (both call into omni_core::validate under the hood) - see
    // crates/omni-wasm.
    await init();
    wasmReady = true;
  });

  function validate(): boolean {
    error = "";
    try {
      validate_camera_name(name);
    } catch (e) {
      error = (e as Error).message;
      return false;
    }
    const trimmed = url.trim();
    if (!trimmed) {
      error = "RTSP URL must not be empty";
      return false;
    }
    if (!trimmed.startsWith("rtsp://")) {
      error = "RTSP URL must start with rtsp://";
      return false;
    }
    return true;
  }

  async function submit() {
    if (!validate()) return;
    submitting = true;
    try {
      await createRtspCamera(name.trim(), url.trim());
      dispatch("created");
      dispatch("close");
    } catch (e) {
      error = (e as Error).message;
    } finally {
      submitting = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") dispatch("close");
  }
</script>

<svelte:window on:keydown={onKeydown} />

<div class="backdrop" role="presentation" on:click={() => dispatch("close")}>
  <div class="modal" role="dialog" aria-modal="true" aria-label="Add RTSP camera" on:click|stopPropagation>
    <h2>Add RTSP camera</h2>
    <p class="hint">
      USB cameras are detected automatically. Use this to add a network camera by RTSP URL.
    </p>
    <label>
      Name
      <input bind:value={name} placeholder="Front Door" disabled={!wasmReady} />
    </label>
    <label>
      RTSP URL
      <input bind:value={url} placeholder="rtsp://192.168.1.50:554/stream1" disabled={!wasmReady} />
    </label>
    {#if error}
      <p class="error">{error}</p>
    {/if}
    <div class="actions">
      <button class="secondary" on:click={() => dispatch("close")}>Cancel</button>
      <button class="primary" on:click={submit} disabled={submitting || !wasmReady}>
        {submitting ? "Adding…" : "Add camera"}
      </button>
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
  }
  .modal {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.5rem;
    width: 360px;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  h2 {
    margin: 0;
    font-size: 1.1rem;
  }
  .hint {
    margin: 0;
    color: var(--text-dim);
    font-size: 0.8rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.8rem;
    color: var(--text-dim);
  }
  input {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.6rem;
    color: var(--text);
    font-size: 0.9rem;
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
  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .secondary {
    background: transparent;
    color: var(--text-dim);
    border: 1px solid var(--border);
  }
</style>
