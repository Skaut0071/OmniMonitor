<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import type { MotionEvent, RecordingInfo } from "./api";

  export let recordings: RecordingInfo[];
  export let events: MotionEvent[];
  export let segmentSeconds: number;

  const dispatch = createEventDispatcher<{
    seek: { recording: RecordingInfo; offsetSeconds: number };
  }>();

  const DAY_SECONDS = 86_400;

  function dayKey(iso: string): string {
    const d = new Date(iso);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  }

  function dayLabel(key: string): string {
    const [y, m, d] = key.split("-").map(Number);
    return new Date(y, m - 1, d).toLocaleDateString(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
    });
  }

  function secondsSinceMidnight(iso: string): number {
    const d = new Date(iso);
    return d.getHours() * 3600 + d.getMinutes() * 60 + d.getSeconds();
  }

  function percentOfDay(iso: string): number {
    return (secondsSinceMidnight(iso) / DAY_SECONDS) * 100;
  }

  // Recordings are sorted oldest-first here regardless of how the parent
  // sorts its own list - the timeline always reads left-to-right as
  // "earlier in the day".
  $: sortedRecordings = [...recordings].sort((a, b) => a.started_at.localeCompare(b.started_at));
  $: dayKeys = [...new Set(sortedRecordings.map((r) => dayKey(r.started_at)))];

  let selectedDay: string | null = null;
  // Default to the most recent day that actually has recordings, but
  // only once (don't fight the user's own prev/next navigation on every
  // reactive re-run as new segments keep appearing while recording).
  let defaultedDay = false;
  $: if (!defaultedDay && dayKeys.length > 0) {
    selectedDay = dayKeys[dayKeys.length - 1];
    defaultedDay = true;
  }

  $: dayIndex = selectedDay ? dayKeys.indexOf(selectedDay) : -1;
  $: dayRecordings = sortedRecordings.filter((r) => dayKey(r.started_at) === selectedDay);
  $: dayEvents = events.filter((e) => dayKey(e.started_at) === selectedDay);

  const HOUR_MARKS = [0, 3, 6, 9, 12, 15, 18, 21, 24];

  function widthPercent(): number {
    return Math.max((segmentSeconds / DAY_SECONDS) * 100, 0.15); // floor so short segments stay clickable/visible
  }

  function eventWidthPercent(ev: MotionEvent): number {
    if (!ev.ended_at) return 0.2;
    const secs = (new Date(ev.ended_at).getTime() - new Date(ev.started_at).getTime()) / 1000;
    return Math.max((secs / DAY_SECONDS) * 100, 0.2);
  }

  function onTrackClick(e: MouseEvent) {
    if (!selectedDay) return;
    const track = e.currentTarget as HTMLElement;
    const rect = track.getBoundingClientRect();
    const fraction = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const clickedSeconds = fraction * DAY_SECONDS;

    const hit = dayRecordings.find((r) => {
      const startSecs = secondsSinceMidnight(r.started_at);
      return clickedSeconds >= startSecs && clickedSeconds < startSecs + segmentSeconds;
    });
    if (hit) {
      const offsetSeconds = clickedSeconds - secondsSinceMidnight(hit.started_at);
      dispatch("seek", { recording: hit, offsetSeconds });
    }
  }
</script>

<div class="timeline">
  {#if dayKeys.length === 0}
    <p class="hint">No recordings to show on a timeline yet.</p>
  {:else}
    <div class="day-nav">
      <button
        class="ghost"
        disabled={dayIndex <= 0}
        on:click={() => (selectedDay = dayKeys[dayIndex - 1])}>‹ Earlier</button
      >
      <span class="day-label">{selectedDay ? dayLabel(selectedDay) : ""}</span>
      <button
        class="ghost"
        disabled={dayIndex === -1 || dayIndex >= dayKeys.length - 1}
        on:click={() => (selectedDay = dayKeys[dayIndex + 1])}>Later ›</button
      >
    </div>

    <div class="hour-marks">
      {#each HOUR_MARKS as h (h)}
        <span style="left: {(h / 24) * 100}%">{h}</span>
      {/each}
    </div>

    <!-- svelte-ignore a11y-click-events-have-key-events -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div class="track" on:click={onTrackClick}>
      {#each dayRecordings as rec (rec.filename)}
        <div
          class="segment"
          title={new Date(rec.started_at).toLocaleTimeString()}
          style="left: {percentOfDay(rec.started_at)}%; width: {widthPercent()}%;"
        ></div>
      {/each}
      {#each dayEvents as ev (ev.id)}
        <div
          class="motion-mark"
          title="Motion at {new Date(ev.started_at).toLocaleTimeString()}"
          style="left: {percentOfDay(ev.started_at)}%; width: {eventWidthPercent(ev)}%;"
        ></div>
      {/each}
    </div>
    <p class="hint">
      Click a segment to play it from that point. Times are approximate - see
      docs/ARCHITECTURE.md.
    </p>
  {/if}
</div>

<style>
  .timeline {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .day-nav {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
  }
  .day-label {
    font-size: 0.82rem;
    font-weight: 600;
    color: var(--text);
    min-width: 9rem;
    text-align: center;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.25rem 0.6rem;
    font-size: 0.75rem;
    cursor: pointer;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-dim);
  }
  .ghost:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .hour-marks {
    position: relative;
    height: 0.9rem;
    font-size: 0.65rem;
    color: var(--text-dim);
  }
  .hour-marks span {
    position: absolute;
    transform: translateX(-50%);
  }
  .track {
    position: relative;
    height: 2.25rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    overflow: hidden;
  }
  .segment {
    position: absolute;
    top: 0.35rem;
    height: 1rem;
    background: var(--accent);
    opacity: 0.6;
    border-radius: 2px;
  }
  .segment:hover {
    opacity: 0.9;
  }
  .motion-mark {
    position: absolute;
    bottom: 0.3rem;
    height: 0.35rem;
    background: #f1c40f;
    border-radius: 2px;
  }
  .hint {
    color: var(--text-dim);
    font-size: 0.72rem;
    margin: 0;
  }
</style>
