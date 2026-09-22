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
  import StatusOverview from "./lib/StatusOverview.svelte";
  import TimelineView from "./lib/TimelineView.svelte";
  import {
    listCameras,
    discoverCameras,
    deleteCamera,
    reorderCameras,
    renameCameraGroup,
    me,
    logout,
    type Camera,
  } from "./lib/api";

  let view: "dashboard" | "timeline" | "status" = "dashboard";
  let selectedGroup: string | null = null; // null = "All"

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
  let mobileNavOpen = false;

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

  // Distinct group names present across all cameras, in first-seen order
  // - the tab list is derived from the data, there's no separate "create
  // a group" step (see Camera.group's docs: a group only exists in the
  // sense that some camera currently has that string set).
  $: groups = [...new Set(cameras.map((c) => c.group).filter((g): g is string => !!g))];
  $: visibleCameras =
    selectedGroup === null ? cameras : cameras.filter((c) => c.group === selectedGroup);

  let renamingGroup: string | null = null;
  let renameValue = "";

  function startRename(name: string) {
    renamingGroup = name;
    renameValue = name;
  }

  async function commitRename() {
    const oldName = renamingGroup;
    renamingGroup = null;
    const newName = renameValue.trim();
    if (!oldName || !newName || newName === oldName) return;
    try {
      await renameCameraGroup(oldName, newName);
      if (selectedGroup === oldName) selectedGroup = newName;
      await refresh();
    } catch (e) {
      loadError = (e as Error).message;
    }
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
  <div class="mobile-topbar">
    <div class="brand">
      <span class="brand-mark">●</span>
      <span class="brand-name">OmniMonitor</span>
    </div>
    <button
      class="hamburger"
      aria-label="Menu"
      aria-expanded={mobileNavOpen}
      on:click={() => (mobileNavOpen = !mobileNavOpen)}
    >
      ☰
    </button>
  </div>

  {#if mobileNavOpen}
    <!-- svelte-ignore a11y-click-events-have-key-events -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div class="drawer-backdrop" on:click={() => (mobileNavOpen = false)}></div>
  {/if}

  <div class="layout">
    <aside class="sidebar" class:open={mobileNavOpen}>
      <div class="brand">
        <span class="brand-mark">●</span>
        <span class="brand-name">OmniMonitor</span>
      </div>
      <nav>
        <!-- svelte-ignore a11y-invalid-attribute -->
        <a
          class:active={view === "dashboard"}
          href="#/"
          on:click|preventDefault={() => {
            view = "dashboard";
            mobileNavOpen = false;
          }}>Dashboard</a
        >
        <!-- svelte-ignore a11y-invalid-attribute -->
        <a
          class:active={view === "timeline"}
          href="#/"
          on:click|preventDefault={() => {
            view = "timeline";
            mobileNavOpen = false;
          }}>Timeline</a
        >
        <!-- svelte-ignore a11y-invalid-attribute -->
        <a
          class:active={view === "status"}
          href="#/"
          on:click|preventDefault={() => {
            view = "status";
            mobileNavOpen = false;
          }}>Status</a
        >
      </nav>
      <div class="sidebar-footer">
        <button
          class="ghost"
          on:click={() => {
            runDiscover();
            mobileNavOpen = false;
          }}>Rescan USB cameras</button
        >
        <button
          class="ghost"
          on:click={() => {
            showIgnoredUsbDevices = true;
            mobileNavOpen = false;
          }}>Ignored USB devices</button
        >
        <button
          class="primary"
          on:click={() => {
            showAddModal = true;
            mobileNavOpen = false;
          }}>+ Add camera</button
        >
        <div class="account">
          <span class="username">{username}</span>
          <button
            class="link"
            on:click={() => {
              showChangePassword = true;
              mobileNavOpen = false;
            }}>Change password</button
          >
          <button
            class="link"
            on:click={() => {
              showRtspCredentials = true;
              mobileNavOpen = false;
            }}>RTSP credentials</button
          >
          <button class="link" on:click={signOut}>Sign out</button>
        </div>
      </div>
    </aside>

    <main>
      <header>
        <h1>
          {#if view === "status"}
            Camera status
          {:else if view === "timeline"}
            Timeline
          {:else}
            Cameras
          {/if}
        </h1>
        {#if view === "dashboard"}
          <span class="count"
            >{visibleCameras.length} camera{visibleCameras.length === 1 ? "" : "s"}</span
          >
        {/if}
      </header>

      {#if view === "status"}
        <StatusOverview />
      {:else if view === "timeline"}
        <TimelineView />
      {:else if loading}
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
        {#if groups.length > 0}
          <div class="group-tabs">
            <button class:active={selectedGroup === null} on:click={() => (selectedGroup = null)}
              >All</button
            >
            {#each groups as g (g)}
              {#if renamingGroup === g}
                <!-- svelte-ignore a11y-autofocus -->
                <input
                  class="group-rename"
                  autofocus
                  bind:value={renameValue}
                  on:blur={commitRename}
                  on:keydown={(e) => {
                    if (e.key === "Enter") commitRename();
                    if (e.key === "Escape") renamingGroup = null;
                  }}
                />
              {:else}
                <button class:active={selectedGroup === g} on:click={() => (selectedGroup = g)}>
                  {g}
                  <!-- svelte-ignore a11y-click-events-have-key-events -->
                  <!-- svelte-ignore a11y-no-static-element-interactions -->
                  <span
                    class="rename-icon"
                    title="Rename group"
                    on:click|stopPropagation={() => startRename(g)}>✎</span
                  >
                </button>
              {/if}
            {/each}
          </div>
        {/if}

        <div class="grid">
          {#each visibleCameras as camera (camera.id)}
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
  .mobile-topbar {
    display: none;
  }
  .drawer-backdrop {
    display: none;
  }
  .hamburger {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 6px;
    font-size: 1.1rem;
    line-height: 1;
    padding: 0.4rem 0.6rem;
    cursor: pointer;
  }

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

  /* Below this width the always-visible sidebar becomes a slide-out
     drawer triggered by a hamburger button in a fixed top bar - a fixed
     220px sidebar permanently eating a third or more of a phone's width
     alongside content isn't usable, but the sidebar's own layout/markup
     stays exactly the same, just repositioned and hidden by default. */
  @media (max-width: 760px) {
    .mobile-topbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 0.6rem 1rem;
      background: var(--surface);
      border-bottom: 1px solid var(--border);
      position: sticky;
      top: 0;
      z-index: 40;
    }
    .mobile-topbar .brand {
      padding: 0;
    }
    .layout {
      display: block;
      height: auto;
      min-height: calc(100% - 3.2rem);
    }
    .sidebar {
      position: fixed;
      top: 0;
      bottom: 0;
      left: 0;
      width: min(280px, 84vw);
      z-index: 50;
      transform: translateX(-100%);
      transition: transform 0.2s ease;
      overflow-y: auto;
    }
    .sidebar.open {
      transform: translateX(0);
    }
    .sidebar .brand {
      display: none; /* already shown in .mobile-topbar */
    }
    .drawer-backdrop {
      display: block;
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.5);
      z-index: 45;
    }
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
  @media (max-width: 760px) {
    main {
      padding: 1rem;
    }
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
  .group-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    margin-bottom: 1.1rem;
  }
  .group-tabs button {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    background: var(--surface);
    border: 1px solid var(--border);
    color: var(--text-dim);
    padding: 0.35rem 0.7rem;
    border-radius: 999px;
    font-size: 0.8rem;
  }
  .group-tabs button.active {
    background: var(--surface-2);
    color: var(--text);
    border-color: var(--accent);
  }
  .rename-icon {
    opacity: 0.5;
    font-size: 0.72rem;
  }
  .rename-icon:hover {
    opacity: 1;
  }
  .group-rename {
    background: var(--bg);
    border: 1px solid var(--accent);
    border-radius: 999px;
    padding: 0.35rem 0.7rem;
    font-size: 0.8rem;
    color: var(--text);
    width: 140px;
  }
  .grid {
    display: grid;
    /* min(320px, 100%) rather than a bare 320px: on a viewport narrower
       than 320px + gutters, a fixed minmax would force horizontal
       scrolling instead of just going to a single, full-width column. */
    grid-template-columns: repeat(auto-fill, minmax(min(320px, 100%), 1fr));
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
