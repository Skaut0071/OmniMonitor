<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import RecordingTimeline from "./RecordingTimeline.svelte";
  import {
    getCamerasStatus,
    listCameras,
    listMotionEvents,
    listRecordings,
    recordingUrl,
    type Camera,
    type CameraStatusInfo,
    type MotionEvent,
    type RecordingInfo,
  } from "./api";
  import {
    connectCameraView,
    SETTINGS_CHANGED_MESSAGE,
    type CameraViewConnection,
    type CameraViewStatus,
  } from "./webrtc-view";

  let cameras: Camera[] = [];
  let statuses: CameraStatusInfo[] = [];
  let selectedId: string | null = null;
  let recordings: RecordingInfo[] = [];
  let events: MotionEvent[] = [];

  // "live" watches the camera's real-time WebRTC feed (like the dashboard
  // tiles); "scrub" shows a frozen frame from a recorded segment, driven
  // by the timeline below the video - this is what lets a viewer rewind
  // straight from this tab instead of opening the Recordings modal and
  // playing through segments one at a time. One persistent <video>
  // element is reused across both (swapping `srcObject` vs `src`
  // imperatively) rather than letting Svelte recreate the element on
  // every mode change, which would otherwise reload/flicker on each wheel
  // tick while scrubbing.
  let mode: "live" | "scrub" = "live";
  let videoEl: HTMLVideoElement | undefined;
  let playbackRecording: RecordingInfo | null = null;
  let pendingSeekSeconds: number | null = null;
  let scrubTimestamp: string | null = null;
  let status: CameraViewStatus | "idle" = "idle";
  let errorMessage = "";
  let connection: CameraViewConnection | null = null;
  let camerasTimer: ReturnType<typeof setInterval>;
  let detailsTimer: ReturnType<typeof setInterval>;

  $: statusById = new Map(statuses.map((s) => [s.id, s.status]));
  // Only cameras the supervisor currently has a live pipeline for are
  // worth offering here - an offline/errored camera has no live feed, and
  // its recordings are still reachable from the Recordings modal.
  $: onlineCameras = cameras.filter((c) => {
    const st = statusById.get(c.id);
    return st === "online" || st === "streaming";
  });
  $: selectedCamera = cameras.find((c) => c.id === selectedId) ?? null;

  async function refreshCameras() {
    try {
      const [camList, statusList] = await Promise.all([listCameras(), getCamerasStatus()]);
      cameras = camList;
      statuses = statusList;
      if (selectedId && !cameras.some((c) => c.id === selectedId)) {
        disconnectLive();
        selectedId = null;
      }
    } catch {
      // Transient - the next poll will retry.
    }
  }

  async function refreshDetails() {
    if (!selectedId) {
      recordings = [];
      events = [];
      return;
    }
    try {
      const [recs, evs] = await Promise.all([
        listRecordings(selectedId),
        listMotionEvents(selectedId),
      ]);
      recordings = recs;
      events = evs;
    } catch {
      // Transient - the next poll will retry.
    }
  }

  function disconnectLive() {
    connection?.disconnect();
    connection = null;
    status = "idle";
    errorMessage = "";
  }

  function connectLive() {
    if (!selectedId || !videoEl) return;
    if (videoEl.hasAttribute("src")) {
      videoEl.removeAttribute("src");
      videoEl.load();
    }
    errorMessage = "";
    const cameraId = selectedId;
    connection = connectCameraView(cameraId, videoEl, {
      onStatusChange: (s) => (status = s),
      onError: (message) => {
        errorMessage = message;
        // Not a real failure - the server tore this viewer down because
        // the camera's settings changed, so reconnect automatically
        // instead of leaving the timeline view stuck on an error.
        if (message === SETTINGS_CHANGED_MESSAGE && selectedId === cameraId && mode === "live") {
          connection?.disconnect();
          connectLive();
        }
      },
    });
  }

  async function select(id: string) {
    if (selectedId === id) return;
    disconnectLive();
    mode = "live";
    playbackRecording = null;
    selectedId = id;
    await refreshDetails();
    connectLive();
  }

  // Fires on every click *and* every wheel tick from RecordingTimeline -
  // scrubbing needs to feel immediate, so this only reloads the <video>'s
  // `src` when the scrub actually crossed into a different recording
  // file; otherwise it just moves `currentTime` on the element already
  // loaded, which is cheap and doesn't re-buffer.
  function onTimelineScrub(e: CustomEvent<{ recording: RecordingInfo; offsetSeconds: number }>) {
    const { recording, offsetSeconds } = e.detail;
    if (mode !== "scrub") {
      disconnectLive();
      mode = "scrub";
    }
    scrubTimestamp = new Date(
      new Date(recording.started_at).getTime() + offsetSeconds * 1000,
    ).toLocaleString();
    if (!videoEl) return;
    if (videoEl.srcObject) videoEl.srcObject = null;
    if (playbackRecording?.filename === recording.filename) {
      videoEl.currentTime = offsetSeconds;
    } else {
      playbackRecording = recording;
      pendingSeekSeconds = offsetSeconds;
      videoEl.src = recordingUrl(selectedCamera!.id, recording.filename);
      videoEl.load();
    }
  }

  // A freshly (re)loaded recording lands here once, to apply whatever
  // scrub offset was requested when its `src` changed; same-file scrubs
  // afterward set `currentTime` directly in `onTimelineScrub` instead.
  function onScrubVideoLoaded() {
    if (!videoEl) return;
    videoEl.pause(); // frame-scrub, not autoplay-forward through the segment
    if (pendingSeekSeconds != null) {
      videoEl.currentTime = pendingSeekSeconds;
      pendingSeekSeconds = null;
    }
  }

  function goLive() {
    mode = "live";
    playbackRecording = null;
    scrubTimestamp = null;
    connectLive();
  }

  // Newest first, so the most recent motion event is always at the top
  // of the vertical list without the viewer having to scroll down.
  $: sortedEvents = [...events].sort((a, b) => b.started_at.localeCompare(a.started_at));

  // Jumps the scrub position to a motion event clicked in the vertical
  // list, the same way clicking/scrolling its mark on the horizontal
  // timeline would - saves hunting for the right spot on the bar by eye.
  function jumpToEvent(ev: MotionEvent) {
    if (!selectedCamera) return;
    const t = new Date(ev.started_at).getTime();
    const segmentMs = selectedCamera.recording.segment_seconds * 1000;
    const hit = recordings.find((r) => {
      const startMs = new Date(r.started_at).getTime();
      return t >= startMs && t < startMs + segmentMs;
    });
    if (!hit) return;
    const startMs = new Date(hit.started_at).getTime();
    onTimelineScrub(
      new CustomEvent("scrub", { detail: { recording: hit, offsetSeconds: (t - startMs) / 1000 } }),
    );
  }

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
    // Keeps the scrub timeline's segment list current while a camera is
    // selected, so a just-finished segment appears without reselecting.
    detailsTimer = setInterval(refreshDetails, 15_000);
  });

  onDestroy(() => {
    disconnectLive();
    clearInterval(camerasTimer);
    clearInterval(detailsTimer);
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
              <span class="dot" class:live={statusById.get(camera.id) === "streaming"}></span>
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
        <p>Select a camera to watch its feed.</p>
      </div>
    {:else}
      <div class="video-wrap">
        <!-- svelte-ignore a11y-media-has-caption -->
        <video
          bind:this={videoEl}
          autoplay={mode === "live"}
          playsinline
          muted
          on:loadedmetadata={mode === "scrub" ? onScrubVideoLoaded : undefined}
        ></video>
        {#if mode === "live" && status !== "live"}
          <div class="overlay">
            {#if status === "error"}
              <span>⚠ {errorMessage || "stream error"}</span>
            {:else}
              <span>Connecting…</span>
            {/if}
          </div>
        {/if}
        {#if mode === "scrub"}
          {#if scrubTimestamp}
            <span class="scrub-time">{scrubTimestamp}</span>
          {/if}
          <button class="live-badge" on:click={goLive}>⏺ Go live</button>
        {/if}
      </div>
      <div class="feed-header">
        <p class="feed-name">{selectedCamera.name}</p>
        {#if mode === "scrub"}
          <button class="ghost" on:click={goLive}>Back to live</button>
        {/if}
      </div>

      {#if recordings.length === 0}
        <p class="hint">
          No recordings to scrub yet.{selectedCamera.recording.enabled
            ? ""
            : " Recording is turned off for this camera."}
        </p>
      {:else}
        <RecordingTimeline
          {recordings}
          {events}
          segmentSeconds={selectedCamera.recording.segment_seconds}
          on:scrub={onTimelineScrub}
        />
      {/if}
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
            <button class="event-body" on:click={() => jumpToEvent(ev)}>
              <span class="event-time">{formatTime(ev.started_at)}</span>
              <span class="event-duration">{durationLabel(ev)}</span>
            </button>
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
    gap: 0.6rem;
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
  .live-badge {
    position: absolute;
    top: 0.6rem;
    right: 0.6rem;
    background: rgba(0, 0, 0, 0.65);
    border: 1px solid rgba(255, 255, 255, 0.25);
    color: #fff;
    border-radius: 999px;
    padding: 0.3rem 0.7rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .scrub-time {
    position: absolute;
    bottom: 0.6rem;
    left: 0.6rem;
    background: rgba(0, 0, 0, 0.65);
    color: #fff;
    border-radius: 6px;
    padding: 0.25rem 0.6rem;
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
    pointer-events: none;
  }
  .feed-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .feed-name {
    margin: 0;
    font-weight: 600;
    font-size: 0.9rem;
    color: var(--text);
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-dim);
    border-radius: 6px;
    padding: 0.3rem 0.7rem;
    font-size: 0.75rem;
    cursor: pointer;
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
    background: transparent;
    border: none;
    padding: 0;
    text-align: left;
    cursor: pointer;
  }
  .event-time {
    font-size: 0.8rem;
    color: var(--text);
  }
  .event-body:hover .event-time {
    color: var(--accent);
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
