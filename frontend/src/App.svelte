<script lang="ts">
  import { onMount } from "svelte";
  import CameraTile from "./lib/CameraTile.svelte";
  import AddCameraModal from "./lib/AddCameraModal.svelte";
  import { listCameras, discoverCameras, deleteCamera, type Camera } from "./lib/api";

  let cameras: Camera[] = [];
  let loading = true;
  let showAddModal = false;
  let loadError = "";

  async function refresh() {
    try {
      cameras = await listCameras();
      loadError = "";
    } catch (e) {
      loadError = (e as Error).message;
    } finally {
      loading = false;
    }
  }

  async function runDiscover() {
    try {
      cameras = await discoverCameras();
    } catch (e) {
      loadError = (e as Error).message;
    }
  }

  async function remove(id: string) {
    await deleteCamera(id);
    await refresh();
  }

  onMount(refresh);
</script>

<div class="layout">
  <aside class="sidebar">
    <div class="brand">
      <span class="brand-mark">●</span>
      <span class="brand-name">OmniMonitor</span>
    </div>
    <nav>
      <a class="active" href="#/">Dashboard</a>
    </nav>
    <div class="sidebar-footer">
      <button class="ghost" on:click={runDiscover}>Rescan USB cameras</button>
      <button class="primary" on:click={() => (showAddModal = true)}>+ Add camera</button>
    </div>
  </aside>

  <main>
    <header>
      <h1>Cameras</h1>
      <span class="count">{cameras.length} camera{cameras.length === 1 ? "" : "s"}</span>
    </header>

    {#if loading}
      <p class="hint">Loading…</p>
    {:else if loadError}
      <p class="error">{loadError}</p>
    {:else if cameras.length === 0}
      <div class="empty">
        <p>No cameras yet.</p>
        <p class="hint">
          Plug in a USB camera and click "Rescan USB cameras", or add an RTSP camera.
        </p>
      </div>
    {:else}
      <div class="grid">
        {#each cameras as camera (camera.id)}
          <div class="grid-item">
            <CameraTile {camera} />
            <button class="remove" title="Remove camera" on:click={() => remove(camera.id)}
              >✕</button
            >
          </div>
        {/each}
      </div>
    {/if}
  </main>
</div>

{#if showAddModal}
  <AddCameraModal on:close={() => (showAddModal = false)} on:created={refresh} />
{/if}

<style>
  .layout {
    display: flex;
    height: 100%;
  }
  .sidebar {
    width: 220px;
    flex-shrink: 0;
    background: var(--surface);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 1rem;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 700;
    font-size: 1.05rem;
    padding: 0.5rem 0.25rem 1.5rem;
  }
  .brand-mark {
    color: var(--accent);
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    flex: 1;
  }
  nav a {
    color: var(--text-dim);
    text-decoration: none;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    font-size: 0.9rem;
  }
  nav a.active {
    background: var(--surface-2);
    color: var(--text);
  }
  .sidebar-footer {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  main {
    flex: 1;
    padding: 1.5rem 2rem;
    overflow-y: auto;
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 0.75rem;
    margin-bottom: 1.25rem;
  }
  h1 {
    margin: 0;
    font-size: 1.3rem;
  }
  .count {
    color: var(--text-dim);
    font-size: 0.85rem;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: 1rem;
  }
  .grid-item {
    position: relative;
  }
  .remove {
    position: absolute;
    top: 0.6rem;
    right: 0.6rem;
    z-index: 5;
    background: rgba(0, 0, 0, 0.5);
    color: #fff;
    border: none;
    border-radius: 999px;
    width: 22px;
    height: 22px;
    cursor: pointer;
    font-size: 0.7rem;
    line-height: 1;
    opacity: 0;
    transition: opacity 0.15s;
  }
  .grid-item:hover .remove {
    opacity: 1;
  }
  .hint,
  .error {
    color: var(--text-dim);
    font-size: 0.9rem;
  }
  .error {
    color: #e74c3c;
  }
  .empty {
    color: var(--text-dim);
    padding: 3rem 0;
    text-align: center;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.5rem 0.9rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .primary {
    background: var(--accent);
    color: #fff;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-dim);
  }
</style>
