<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import {
    deleteRecording,
    listRecordings,
    recordingUrl,
    type Camera,
    type RecordingInfo,
  } from "./api";

  export let camera: Camera;
  const dispatch = createEventDispatcher();

  let recordings: RecordingInfo[] = [];
  let loading = true;
  let loadError = "";
  let selected: RecordingInfo | null = null;

  async function refresh() {
    loading = true;
    try {
      recordings = (await listRecordings(camera.id)).reverse(); // newest first
      loadError = "";
      if (!selected && recordings.length > 0) {
        selected = recordings[0];
      }
    } catch (e) {
      loadError = (e as Error).message;
    } finally {
      loading = false;
    }
  }

  async function remove(rec: RecordingInfo) {
    await deleteRecording(camera.id, rec.filename);
    if (selected?.filename === rec.filename) selected = null;
    await refresh();
  }

  function formatSize(bytes: number): string {
    if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
    if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(1)} MB`;
    return `${(bytes / 1000).toFixed(0)} KB`;
  }

  function formatTime(iso: string): string {
    return new Date(iso).toLocaleString();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") dispatch("close");
  }

  onMount(refresh);
</script>

<svelte:window on:keydown={onKeydown} />

<div class="backdrop" role="presentation" on:click={() => dispatch("close")}>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="{camera.name} recordings"
    on:click|stopPropagation
  >
    <div class="header">
      <h2>{camera.name} - Recordings</h2>
      <button class="icon" on:click={() => dispatch("close")} aria-label="Close">✕</button>
    </div>

    {#if loading}
      <p class="hint">Loading…</p>
    {:else if loadError}
      <p class="error">{loadError}</p>
    {:else if recordings.length === 0}
      <p class="hint">
        No recordings yet.{camera.recording.enabled
          ? ""
          : " Recording is turned off for this camera."}
      </p>
    {:else}
      <div class="body">
        <div class="list">
          {#each recordings as rec (rec.filename)}
            <div class="item" class:active={selected?.filename === rec.filename}>
              <button class="item-main" on:click={() => (selected = rec)}>
                <span class="filename">{formatTime(rec.modified)}</span>
                <span class="meta">{formatSize(rec.size_bytes)}</span>
              </button>
              <button class="remove" title="Delete" on:click={() => remove(rec)}>✕</button>
            </div>
          {/each}
        </div>
        <div class="player">
          {#if selected}
            <!-- svelte-ignore a11y-media-has-caption -->
            <video src={recordingUrl(camera.id, selected.filename)} controls autoplay></video>
            <a class="download" href={recordingUrl(camera.id, selected.filename)} download
              >Download segment</a
            >
          {:else}
            <p class="hint">Select a recording to play it back.</p>
          {/if}
        </div>
      </div>
    {/if}
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
    padding: 1.25rem;
    width: min(760px, 100%);
    max-height: 85vh;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: 1.05rem;
  }
  .icon {
    background: transparent;
    border: none;
    color: var(--text-dim);
    cursor: pointer;
    font-size: 0.9rem;
  }
  .body {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 1rem;
    min-height: 0;
    overflow: hidden;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    overflow-y: auto;
  }
  .item {
    display: flex;
    align-items: stretch;
    border-radius: 6px;
    overflow: hidden;
    background: var(--surface-2);
  }
  .item.active {
    outline: 1px solid var(--accent);
  }
  .item-main {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.1rem;
    background: transparent;
    border: none;
    color: var(--text);
    padding: 0.5rem 0.6rem;
    cursor: pointer;
    text-align: left;
    font-size: 0.78rem;
  }
  .filename {
    font-weight: 600;
  }
  .meta {
    color: var(--text-dim);
    font-size: 0.72rem;
  }
  .remove {
    background: transparent;
    border: none;
    color: var(--text-dim);
    cursor: pointer;
    padding: 0 0.6rem;
    font-size: 0.7rem;
  }
  .remove:hover {
    color: #e74c3c;
  }
  .player {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    min-width: 0;
  }
  .player video {
    width: 100%;
    max-height: 55vh;
    background: #000;
    border-radius: 8px;
  }
  .download {
    align-self: flex-start;
    color: var(--accent);
    font-size: 0.8rem;
    text-decoration: none;
  }
  .hint,
  .error {
    color: var(--text-dim);
    font-size: 0.85rem;
    margin: 0;
  }
  .error {
    color: #e74c3c;
  }
</style>
