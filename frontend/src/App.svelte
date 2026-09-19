<script lang="ts">
  import { onMount } from "svelte";
  import CameraTile from "./lib/CameraTile.svelte";
  import AddCameraModal from "./lib/AddCameraModal.svelte";
  import CameraSettingsModal from "./lib/CameraSettingsModal.svelte";
  import RecordingsModal from "./lib/RecordingsModal.svelte";
  import Login from "./lib/Login.svelte";
  import ChangePasswordModal from "./lib/ChangePasswordModal.svelte";
  import RtspCredentialsModal from "./lib/RtspCredentialsModal.svelte";
  import ExpandedCameraModal from "./lib/ExpandedCameraModal.svelte";
  import IgnoredUsbDevicesModal from "./lib/IgnoredUsbDevicesModal.svelte";
  import {
    listCameras,
    discoverCameras,
    deleteCamera,
    reorderCameras,
    me,
    logout,
    type Camera,
  } from "./lib/api";

  let authChecked = false;
  let username: string | null = null;

  let cameras: Camera[] = [];
  let loading = true;
  let showAddModal = false;
  let showChangePassword = false;
  let showRtspCredentials = false;
  let loadError = "";
  let settingsCamera: Camera | null = null;
  let recordingsCamera: Camera | null = null;
  let expandedCamera: Camera | null = null;
  let showIgnoredUsbDevices = false;

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

  let draggedId: string | null = null;

  function onDragStart(id: string) {
    draggedId = id;
  }

  function onDragOver(e: DragEvent) {
    // Required for `drop` to fire at all - browsers otherwise reject
    // the element as a drop target.
    e.preventDefault();
  }

  async function onDrop(targetId: string) {
    if (draggedId === null || draggedId === targetId) {
      draggedId = null;
      return;
    }
    const fromIndex = cameras.findIndex((c) => c.id === draggedId);
    const toIndex = cameras.findIndex((c) => c.id === targetId);
    if (fromIndex === -1 || toIndex === -1) {
      draggedId = null;
      return;
    }
    const reordered = [...cameras];
    const [moved] = reordered.splice(fromIndex, 1);
    reordered.splice(toIndex, 0, moved);
    cameras = reordered; // optimistic - reflects the drop immediately
    draggedId = null;
    try {
      await reorderCameras(reordered.map((c) => c.id));
    } catch (e) {
      loadError = (e as Error).message;
      await refresh(); // fall back to the server's actual order
    }
  }

  async function checkAuth() {
    const session = await me();
    username = session?.username ?? null;
    authChecked = true;
    if (username) refresh();
  }

  async function onLoggedIn() {
    await checkAuth();
  }

  async function signOut() {
    await logout();
    username = null;
    cameras = [];
  }

  onMount(checkAuth);
</script>

{#if !authChecked}
  <div class="boot-hint">Loading…</div>
{:else if !username}
  <Login on:loggedIn={onLoggedIn} />
{:else}
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
        <button class="ghost" on:click={() => (showIgnoredUsbDevices = true)}
          >Ignored USB devices</button
        >
        <button class="primary" on:click={() => (showAddModal = true)}>+ Add camera</button>
        <div class="account">
          <span class="username">{username}</span>
          <button class="link" on:click={() => (showChangePassword = true)}>Change password</button>
          <button class="link" on:click={() => (showRtspCredentials = true)}>RTSP credentials</button>
          <button class="link" on:click={signOut}>Sign out</button>
        </div>
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
            <div
              class="grid-item"
              class:dragging={draggedId === camera.id}
              draggable="true"
              on:dragstart={() => onDragStart(camera.id)}
              on:dragover={onDragOver}
              on:drop={() => onDrop(camera.id)}
            >
              <CameraTile
                {camera}
                on:remove={() => remove(camera.id)}
                on:settings={() => (settingsCamera = camera)}
                on:recordings={() => (recordingsCamera = camera)}
                on:expand={() => (expandedCamera = camera)}
              />
            </div>
          {/each}
        </div>
      {/if}
    </main>
  </div>

  {#if showAddModal}
    <AddCameraModal on:close={() => (showAddModal = false)} on:created={refresh} />
  {/if}

  {#if settingsCamera}
    <CameraSettingsModal
      camera={settingsCamera}
      on:close={() => (settingsCamera = null)}
      on:updated={refresh}
    />
  {/if}

  {#if recordingsCamera}
    <RecordingsModal camera={recordingsCamera} on:close={() => (recordingsCamera = null)} />
  {/if}

  {#if showChangePassword}
    <ChangePasswordModal on:close={() => (showChangePassword = false)} />
  {/if}

  {#if showRtspCredentials}
    <RtspCredentialsModal on:close={() => (showRtspCredentials = false)} />
  {/if}

  {#if expandedCamera}
    <ExpandedCameraModal camera={expandedCamera} on:close={() => (expandedCamera = null)} />
  {/if}

  {#if showIgnoredUsbDevices}
    <IgnoredUsbDevicesModal
      on:close={() => (showIgnoredUsbDevices = false)}
      on:restored={runDiscover}
    />
  {/if}
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
  .boot-hint {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-dim);
    font-size: 0.9rem;
  }
  .account {
    margin-top: 1rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .username {
    font-size: 0.8rem;
    color: var(--text-dim);
    margin-bottom: 0.2rem;
  }
  .link {
    background: transparent;
    border: none;
    color: var(--text-dim);
    text-align: left;
    padding: 0.2rem 0;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .link:hover {
    color: var(--text);
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
    cursor: grab;
  }
  .grid-item.dragging {
    opacity: 0.4;
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
