<script lang="ts">
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import type { Camera } from "./api";
  import { connectCameraView, type CameraViewConnection, type CameraViewStatus } from "./webrtc-view";

  export let camera: Camera;

  const dispatch = createEventDispatcher();

  let videoEl: HTMLVideoElement;
  let status: CameraViewStatus | "idle" = "idle";
  let errorMessage = "";
  let connection: CameraViewConnection | null = null;

  // Live-view-only zoom: a CSS transform on the <video> element, browser
  // side. Doesn't touch the pipeline, recordings, or the RTSP re-serve -
  // just what this viewer happens to be looking at, the same way
  // pinch-zooming a photo viewer doesn't edit the photo. Panning only
  // matters once zoomed in, since at 1x the video already fills the
  // frame exactly.
  let zoom = 1;
  const MIN_ZOOM = 1;
  const MAX_ZOOM = 4;
  let panX = 0;
  let panY = 0;
  let dragging = false;
  let dragStartX = 0;
  let dragStartY = 0;
  let panStartX = 0;
  let panStartY = 0;

  function clampPan() {
    // Don't let panning drag the zoomed video past its own edge into
    // visible letterboxing - the max offset shrinks as zoom approaches 1.
    const maxOffsetPercent = ((zoom - 1) / zoom) * 50;
    panX = Math.max(-maxOffsetPercent, Math.min(maxOffsetPercent, panX));
    panY = Math.max(-maxOffsetPercent, Math.min(maxOffsetPercent, panY));
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const delta = e.deltaY > 0 ? -0.25 : 0.25;
    zoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, zoom + delta));
    if (zoom === 1) {
      panX = 0;
      panY = 0;
    } else {
      clampPan();
    }
  }

  function onPointerDown(e: PointerEvent) {
    if (zoom === 1) return;
    dragging = true;
    dragStartX = e.clientX;
    dragStartY = e.clientY;
    panStartX = panX;
    panStartY = panY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    // Percent-of-element-size movement, so drag speed feels consistent
    // regardless of the modal's actual pixel size.
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    panX = panStartX + ((e.clientX - dragStartX) / rect.width) * 100;
    panY = panStartY + ((e.clientY - dragStartY) / rect.height) * 100;
    clampPan();
  }

  function onPointerUp() {
    dragging = false;
  }

  function resetZoom() {
    zoom = 1;
    panX = 0;
    panY = 0;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") dispatch("close");
  }

  onMount(() => {
    connection = connectCameraView(camera.id, videoEl, {
      onStatusChange: (s) => (status = s),
      onError: (message) => (errorMessage = message),
    });
  });

  onDestroy(() => {
    connection?.disconnect();
  });
</script>

<svelte:window on:keydown={onKeydown} />

<!-- svelte-ignore a11y-click-events-have-key-events -->
<!-- svelte-ignore a11y-no-static-element-interactions -->
<div class="backdrop" on:click={() => dispatch("close")}>
  <!-- svelte-ignore a11y-click-events-have-key-events -->
  <!-- svelte-ignore a11y-no-static-element-interactions -->
  <div class="modal" on:click|stopPropagation>
    <div class="header">
      <span class="name">{camera.name}</span>
      <div class="header-actions">
        {#if zoom > 1}
          <span class="zoom-level">{zoom.toFixed(2)}x</span>
          <button class="ghost" on:click={resetZoom}>Reset zoom</button>
        {/if}
        <button class="ghost" on:click={() => dispatch("close")}>✕ Close</button>
      </div>
    </div>

    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div
      class="video-wrap"
      class:zoomed={zoom > 1}
      on:wheel={onWheel}
      on:pointerdown={onPointerDown}
      on:pointermove={onPointerMove}
      on:pointerup={onPointerUp}
      on:pointerleave={onPointerUp}
      on:dblclick={resetZoom}
    >
      <!-- svelte-ignore a11y-media-has-caption -->
      <video
        bind:this={videoEl}
        autoplay
        playsinline
        muted
        style="transform: scale({zoom}) translate({panX}%, {panY}%);"
      ></video>
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
    <p class="hint">Scroll to zoom, drag to pan, double-click to reset. Live view only.</p>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.85);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 60;
    padding: 2rem;
  }
  .modal {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1rem;
    width: 100%;
    max-width: 1100px;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .name {
    font-weight: 600;
    font-size: 1rem;
    color: var(--text);
  }
  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .zoom-level {
    font-size: 0.78rem;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }
  .video-wrap {
    position: relative;
    background: #000;
    aspect-ratio: 16 / 9;
    overflow: hidden;
    border-radius: 6px;
    touch-action: none;
  }
  .video-wrap.zoomed {
    cursor: grab;
  }
  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
    transform-origin: center center;
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
  .hint {
    margin: 0;
    color: var(--text-dim);
    font-size: 0.75rem;
    text-align: center;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.4rem 0.8rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-dim);
  }
</style>
