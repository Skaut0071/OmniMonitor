<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import { listIgnoredUsbDevices, unignoreUsbDevice, type IgnoredUsbDevice } from "./api";

  const dispatch = createEventDispatcher();

  let devices: IgnoredUsbDevice[] | null = null;
  let error = "";
  let restoring = "";

  async function load() {
    try {
      devices = await listIgnoredUsbDevices();
    } catch (e) {
      error = (e as Error).message;
    }
  }

  async function restore(devicePath: string) {
    restoring = devicePath;
    error = "";
    try {
      await unignoreUsbDevice(devicePath);
      await load();
      // The device itself isn't re-added until a rescan actually happens.
      dispatch("restored");
    } catch (e) {
      error = (e as Error).message;
    } finally {
      restoring = "";
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") dispatch("close");
  }

  onMount(load);
</script>

<svelte:window on:keydown={onKeydown} />

<div class="backdrop" role="presentation" on:click={() => dispatch("close")}>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Ignored USB devices"
    on:click|stopPropagation
  >
    <h2>Ignored USB devices</h2>
    <p class="hint">
      Deleting a USB camera also stops it from being auto-detected again (e.g. if it's actually
      used for something else on this machine) - undo that here. Restoring a device doesn't add
      it back immediately; click "Rescan USB cameras" afterward.
    </p>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    {#if devices === null}
      <p class="hint">Loading…</p>
    {:else if devices.length === 0}
      <p class="hint">No ignored devices.</p>
    {:else}
      <ul class="device-list">
        {#each devices as device (device.device_path)}
          <li>
            <span class="path">{device.device_path}</span>
            <button
              class="ghost"
              on:click={() => restore(device.device_path)}
              disabled={restoring === device.device_path}
            >
              {restoring === device.device_path ? "Restoring…" : "Restore"}
            </button>
          </li>
        {/each}
      </ul>
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
  }
  .modal {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.5rem;
    width: 420px;
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
  .error {
    color: #e74c3c;
    font-size: 0.8rem;
    margin: 0;
  }
  .device-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    max-height: 220px;
    overflow-y: auto;
  }
  .device-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.45rem 0.6rem;
  }
  .path {
    font-family: monospace;
    font-size: 0.85rem;
    color: var(--text);
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
  }
  .ghost:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
