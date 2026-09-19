<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { getCamerasStatus, type CameraStatusInfo } from "./api";

  let statuses: CameraStatusInfo[] | null = null;
  let error = "";
  let loading = false;
  let refreshTimer: ReturnType<typeof setInterval> | null = null;

  async function load() {
    loading = true;
    try {
      statuses = await getCamerasStatus();
      error = "";
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  }

  function statusLabel(status: CameraStatusInfo["status"]): string {
    switch (status) {
      case "streaming":
        return "Streaming";
      case "error":
        return "Error";
      case "online":
        return "Online";
      case "offline":
        return "Offline";
    }
  }

  onMount(() => {
    load();
    // Idle/offline cameras are re-probed fresh on every call (see the
    // backend's docs) - not cheap enough to poll tightly, but a slow
    // background refresh keeps this page from going stale if left open.
    refreshTimer = setInterval(load, 30_000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<div class="status-page">
  <div class="toolbar">
    <button class="ghost" on:click={load} disabled={loading}>
      {loading ? "Checking…" : "Refresh"}
    </button>
  </div>

  {#if error}
    <p class="error">{error}</p>
  {:else if statuses === null}
    <p class="hint">Loading…</p>
  {:else if statuses.length === 0}
    <p class="hint">No cameras yet.</p>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Camera</th>
          <th>Type</th>
          <th>Status</th>
        </tr>
      </thead>
      <tbody>
        {#each statuses as s (s.id)}
          <tr>
            <td>{s.name}</td>
            <td><span class="kind-badge">{s.kind === "usb" ? "USB" : "Network"}</span></td>
            <td>
              <span class="status-badge status-{s.status}">{statusLabel(s.status)}</span>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .status-page {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  .toolbar {
    display: flex;
    justify-content: flex-end;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.4rem 0.9rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-dim);
  }
  .ghost:disabled {
    opacity: 0.6;
    cursor: default;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
  }
  th,
  td {
    text-align: left;
    padding: 0.6rem 0.9rem;
    font-size: 0.85rem;
    border-bottom: 1px solid var(--border);
  }
  th {
    color: var(--text-dim);
    font-weight: 600;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  tr:last-child td {
    border-bottom: none;
  }
  .kind-badge {
    font-size: 0.72rem;
    color: var(--text-dim);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.1rem 0.5rem;
  }
  .status-badge {
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.15rem 0.55rem;
    border-radius: 999px;
  }
  .status-streaming {
    background: rgba(46, 204, 113, 0.15);
    color: #2ecc71;
  }
  .status-online {
    background: rgba(52, 152, 219, 0.15);
    color: #3498db;
  }
  .status-error {
    background: rgba(231, 76, 60, 0.15);
    color: #e74c3c;
  }
  .status-offline {
    background: rgba(149, 165, 166, 0.15);
    color: #95a5a6;
  }
  .hint,
  .error {
    color: var(--text-dim);
    font-size: 0.85rem;
  }
  .error {
    color: #e74c3c;
  }
</style>
