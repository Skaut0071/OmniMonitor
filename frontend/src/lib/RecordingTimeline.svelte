<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import type { MotionEvent, RecordingInfo } from "./api";

  export let recordings: RecordingInfo[];
  export let events: MotionEvent[];
  export let segmentSeconds: number;

  const dispatch = createEventDispatcher<{
    scrub: { recording: RecordingInfo; offsetSeconds: number };
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
  // Unlabeled hourly tick lines in the track itself (labels only every 3h
  // above it) - purely a visual aid for aiming a click/scrub at a
  // specific time on a now-taller, easier-to-target bar.
  const MINOR_TICKS = Array.from({ length: 23 }, (_, i) => i + 1);

  function widthPercent(): number {
    return Math.max((segmentSeconds / DAY_SECONDS) * 100, 0.15); // floor so short segments stay clickable/visible
  }

  function eventWidthPercent(ev: MotionEvent): number {
    if (!ev.ended_at) return 0.2;
    const secs = (new Date(ev.ended_at).getTime() - new Date(ev.started_at).getTime()) / 1000;
    return Math.max((secs / DAY_SECONDS) * 100, 0.2);
  }

  // The playhead: seconds-since-midnight on `selectedDay` that the video
  // is currently showing a frame from. Driven by both a click (jump
  // straight there) and a wheel scroll (nudge it, like a jog wheel) -
  // either way it's the single source of truth for where the preview
  // frame comes from, so both interactions stay in sync with what's drawn.
  let cursorSeconds: number | null = null;

  function recordingAt(seconds: number): { recording: RecordingInfo; offsetSeconds: number } | null {
    const hit = dayRecordings.find((r) => {
      const startSecs = secondsSinceMidnight(r.started_at);
      return seconds >= startSecs && seconds < startSecs + segmentSeconds;
    });
    return hit ? { recording: hit, offsetSeconds: seconds - secondsSinceMidnight(hit.started_at) } : null;
  }

  function moveCursorTo(seconds: number) {
    if (!selectedDay) return;
    cursorSeconds = Math.min(DAY_SECONDS - 1, Math.max(0, seconds));
    const hit = recordingAt(cursorSeconds);
    if (hit) dispatch("scrub", hit);
  }

  function onTrackClick(e: MouseEvent) {
    const track = e.currentTarget as HTMLElement;
    const rect = track.getBoundingClientRect();
    const fraction = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    moveCursorTo(fraction * DAY_SECONDS);
  }

  // Scrolling over the timeline scrubs through time directly, like a
  // video editor's jog wheel, instead of the browser scrolling the page -
  // ~15s per wheel notch (100 is a typical Chrome/Firefox deltaY per
  // notch; a full day at 4s/notch felt glacial - see docs/ARCHITECTURE.md),
  // scaling naturally with a trackpad's finer continuous deltas.
  function onTrackWheel(e: WheelEvent) {
    if (!selectedDay) return;
    e.preventDefault();
    const now = new Date();
    const base = cursorSeconds ?? now.getHours() * 3600 + now.getMinutes() * 60 + now.getSeconds();
    moveCursorTo(base + (e.deltaY / 100) * 15);
  }

  $: cursorPercent = cursorSeconds != null ? (cursorSeconds / DAY_SECONDS) * 100 : null;
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
    <div class="track" on:click={onTrackClick} on:wheel={onTrackWheel}>
      {#each MINOR_TICKS as h (h)}
        <div class="tick" style="left: {(h / 24) * 100}%;"></div>
      {/each}
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
      {#if cursorPercent != null}
        <div class="playhead" style="left: {cursorPercent}%;"></div>
      {/if}
    </div>
    <p class="hint">
      Scroll over the timeline to scrub through time, or click to jump straight there. Times are
      approximate - see docs/ARCHITECTURE.md.
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
    height: 1.1rem;
    font-size: 0.75rem;
    color: var(--text-dim);
  }
  .hour-marks span {
    position: absolute;
    transform: translateX(-50%);
  }
  .track {
    position: relative;
    height: 4.25rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    overflow: hidden;
  }
  .tick {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border);
  }
  .segment {
    position: absolute;
    top: 0.6rem;
    height: 2rem;
    background: var(--accent);
    opacity: 0.6;
    border-radius: 2px;
  }
  .segment:hover {
    opacity: 0.9;
  }
  .motion-mark {
    position: absolute;
    bottom: 0.5rem;
    height: 0.5rem;
    background: #f1c40f;
    border-radius: 2px;
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: #fff;
    box-shadow: 0 0 4px rgba(255, 255, 255, 0.7);
    pointer-events: none;
  }
  .hint {
    color: var(--text-dim);
    font-size: 0.72rem;
    margin: 0;
  }
</style>
