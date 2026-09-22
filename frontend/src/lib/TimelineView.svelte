<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { getCamerasStatus, listMotionEvents, type CameraStatusInfo, type MotionEvent } from "./api";
  import {
    connectCameraView,
    SETTINGS_CHANGED_MESSAGE,
    type CameraViewConnection,
    type CameraViewStatus,
  } from "./webrtc-view";

  let cameras: CameraStatusInfo[] = [];
  let selectedId: string | null = null;
  let events: MotionEvent[] = [];
  let videoEl: HTMLVideoElement;
  let status: CameraViewStatus | "idle" = "idle";
  let errorMessage = "";
  let connection: CameraViewConnection | null = null;
  let camerasTimer: ReturnType<typeof setInterval>;
  let eventsTimer: ReturnType<typeof setInterval>;

  // Only cameras the supervisor currently has a live pipeline for are
  // worth offering here - an offline/errored camera has no video feed to
  // show, so it would just be a dead entry in the list.
  $: onlineCameras = cameras.filter((c) => c.status === "online" || c.status === "streaming");
  $: selectedCamera = cameras.find((c) => c.id === selectedId) ?? null;

  async function refreshCameras() {
    try {
      cameras = await getCamerasStatus();
      // If the previously-selected camera went offline, drop the live
      // connection rather than leaving a dead video element around.
      if (selectedId && !cameras.some((c) => c.id === selectedId)) {
        disconnect();
        selectedId = null;
      }
    } catch {
      // Transient - the next poll will retry.
    }
  }

  async function refreshEvents() {
    if (!selectedId) {
      events = [];
      return;
    }
    try {
      events = await listMotionEvents(selectedId);
    } catch {
      // Transient - the next poll will retry.
    }
  }

  function disconnect() {
    connection?.disconnect();
    connection = null;
    status = "idle";
    errorMessage = "";
  }

  function connect() {
    if (!selectedId || !videoEl) return;
    errorMessage = "";
    const cameraId = selectedId;
    connection = connectCameraView(cameraId, videoEl, {
      onStatusChange: (s) => (status = s),
      onError: (message) => {
        errorMessage = message;
        // Not a real failure - the server tore this viewer down because
        // the camera's settings changed, so reconnect automatically
        // instead of leaving the timeline view stuck on an error.
        if (message === SETTINGS_CHANGED_MESSAGE && selectedId === cameraId) {
          connection?.disconnect();
          connect();
        }
      },
    });
  }

  async function select(id: string) {
    if (selectedId === id) return;
    disconnect();
    selectedId = id;
    await refreshEvents();
    connect();
  }

  // Newest first, so the most recent motion event is always at the top
  // of the vertical timeline without the viewer having to scroll down.
  $: sortedEvents = [...events].sort((a, b) => b.started_at.localeCompare(a.started_at));

  function formatTime(iso: string): string {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  function durationLabel(ev: MotionEvent): string {
    if (!ev.ended_at) return "ongoing";
    const secs = Math.round(
      (new Date(ev.ended_at).getTime() - new Date(ev.started_at).getTime()) / 1000,
    );
    if (secs < 60) return `${secs}s`;
    return `${Math.round(secs / 60)}m`;
  }

  onMount(() => {
    refreshCameras();
    camerasTimer = setInterval(refreshCameras, 10_000);
    eventsTimer = setInterval(refreshEvents, 15_000);
  });

  onDestroy(() => {
    disconnect();
    clearInterval(camerasTimer);
    clearInterval(eventsTimer);
  });
</script>

<div class="timeline-view">
  <aside class="cam-list">
    <h2>Online cameras</h2>
    {#if onlineCameras.length === 0}
      <p class="hint">No cameras are currently online.</p>
    {:else}
      <ul>
        {#each onlineCameras as camera (camera.id)}
          <li>
            <button class:active={selectedId === camera.id} on:click={() => select(camera.id)}>
              <span class="dot" class:live={camera.status === "streaming"}></span>
              {camera.name}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </aside>

  <main class="feed">
    {#if !selectedCamera}
      <div class="placeholder">
        <p>Select a camera to watch its live feed.</p>
      </div>
    {:else}
      <div class="video-wrap">
        <!-- svelte-ignore a11y-media-has-caption -->
        <video bind:this={videoEl} autoplay playsinline muted></video>
        {#if status !== "live"}
          <div class="overlay">
            {#if status === "error"}
              <span>⚠ {errorMessage || "stream error"}</span>
            {:else}
              <span>Connecting…</span>
            {/if}
          </div>
        {/if}
      </div>
      <p class="feed-name">{selectedCamera.name}</p>
    {/if}
  </main>

  <aside class="events">
    <h2>Motion events</h2>
    {#if !selectedCamera}
      <p class="hint">Select a camera to see its events.</p>
    {:else if sortedEvents.length === 0}
      <p class="hint">No motion events recorded for this camera yet.</p>
    {:else}
      <ol class="event-track">
        {#each sortedEvents as ev (ev.id)}
          <li>
            <span class="marker" class:ongoing={!ev.ended_at}></span>
            <div class="event-body">
              <span class="event-time">{formatTime(ev.started_at)}</span>
              <span class="event-duration">{durationLabel(ev)}</span>
            </div>
          </li>
        {/each}
      </ol>
    {/if}
  </aside>
</div>

<style>
  .timeline-view {
    display: grid;
    grid-template-columns: 200px 1fr 260px;
    gap: 1rem;
    align-items: start;
  }
  @media (max-width: 900px) {
    .timeline-view {
      grid-template-columns: 1fr;
    }
  }

  h2 {
    margin: 0 0 0.6rem;
    font-size: 0.85rem;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .cam-list,
  .events {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.85rem;
  }
  .cam-list ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .cam-list button {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    border-radius: 6px;
    padding: 0.45rem 0.5rem;
    font-size: 0.85rem;
    color: var(--text-dim);
    cursor: pointer;
  }
  .cam-list button:hover {
    background: var(--surface-2);
  }
  .cam-list button.active {
    background: var(--surface-2);
    color: var(--text);
  }
  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--text-dim);
    flex-shrink: 0;
  }
  .dot.live {
    background: #2ecc71;
  }

  .feed {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .placeholder {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    aspect-ratio: 16 / 9;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-dim);
    font-size: 0.9rem;
  }
  .video-wrap {
    position: relative;
    background: #000;
    aspect-ratio: 16 / 9;
    border-radius: 8px;
    overflow: hidden;
  }
  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-dim);
    font-size: 0.9rem;
    background: rgba(0, 0, 0, 0.35);
  }
  .feed-name {
    margin: 0;
    font-weight: 600;
    font-size: 0.9rem;
    color: var(--text);
  }

  .event-track {
    list-style: none;
    margin: 0;
    padding: 0;
    position: relative;
    max-height: 70vh;
    overflow-y: auto;
  }
  .event-track::before {
    content: "";
    position: absolute;
    left: 0.22rem;
    top: 0.35rem;
    bottom: 0.35rem;
    width: 2px;
    background: var(--border);
  }
  .event-track li {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 0.6rem;
    padding: 0 0 0.9rem 1.2rem;
  }
  .marker {
    position: absolute;
    left: 0;
    top: 0.2rem;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: #f1c40f;
    z-index: 1;
  }
  .marker.ongoing {
    background: #e74c3c;
    box-shadow: 0 0 0 3px rgba(231, 76, 60, 0.25);
  }
  .event-body {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .event-time {
    font-size: 0.8rem;
    color: var(--text);
  }
  .event-duration {
    font-size: 0.72rem;
    color: var(--text-dim);
  }

  .hint {
    color: var(--text-dim);
    font-size: 0.85rem;
    margin: 0;
  }
</style>
