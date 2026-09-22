<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import init, {
    validate_retention,
    validate_segment_seconds,
    validate_sensitivity,
    validate_webhook_url,
    validate_group_name,
    validate_schedule_minute,
  } from "./wasm/omni_wasm.js";
  import { updateCamera, type Camera, type RecordingTrigger, type Rotation } from "./api";

  export let camera: Camera;

  const dispatch = createEventDispatcher();

  type AgeUnit = "minutes" | "hours" | "days" | "months";
  type SizeUnit = "MB" | "GB" | "TB";

  const AGE_UNIT_SECONDS: Record<AgeUnit, number> = {
    minutes: 60,
    hours: 3600,
    days: 86_400,
    // Approximate - a calendar month has no fixed length in seconds.
    months: 30 * 86_400,
  };
  const SIZE_UNIT_BYTES: Record<SizeUnit, number> = {
    MB: 1_000_000,
    GB: 1_000_000_000,
    TB: 1_000_000_000_000,
  };

  let recordingEnabled = camera.recording.enabled;
  let recordingTrigger: RecordingTrigger = camera.recording.trigger;
  let segmentMinutes = camera.recording.segment_seconds / 60;

  let motionEnabled = camera.motion.enabled;
  let motionSensitivity = camera.motion.sensitivity;
  let webhookUrl = camera.motion.webhook_url ?? "";

  let rotation: Rotation = camera.rotation;
  let overlayTimestamp = camera.overlay_timestamp;
  let group = camera.group ?? "";

  let ageEnabled = camera.recording.retention_max_age_secs != null;
  let ageValue = camera.recording.retention_max_age_secs
    ? Math.max(1, Math.round(camera.recording.retention_max_age_secs / 86_400))
    : 7;
  let ageUnit: AgeUnit = "days";

  let sizeEnabled = camera.recording.retention_max_size_bytes != null;
  let sizeValue = camera.recording.retention_max_size_bytes
    ? Math.max(1, Math.round(camera.recording.retention_max_size_bytes / 1_000_000_000))
    : 10;
  let sizeUnit: SizeUnit = "GB";

  const DAY_LABELS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

  function minutesToTime(minutes: number): string {
    const h = Math.floor(minutes / 60)
      .toString()
      .padStart(2, "0");
    const m = (minutes % 60).toString().padStart(2, "0");
    return `${h}:${m}`;
  }

  function timeToMinutes(time: string): number {
    const [h, m] = time.split(":").map(Number);
    return h * 60 + m;
  }

  let scheduleEnabled = camera.recording.schedule.enabled;
  // Copy, not a reference to the prop's array - toggling a day mutates
  // this in place (see the day-toggle button below) and shouldn't touch
  // `camera` until Save is actually clicked.
  let scheduleDays = [...camera.recording.schedule.days];
  let scheduleStartTime = minutesToTime(camera.recording.schedule.start_minute);
  let scheduleEndTime = minutesToTime(camera.recording.schedule.end_minute);

  let error = "";
  let wasmReady = false;
  let saving = false;

  onMount(async () => {
    await init();
    wasmReady = true;
  });

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") dispatch("close");
  }

  async function save() {
    error = "";
    const segmentSeconds = Math.round(segmentMinutes * 60);
    const maxAgeSecs = ageEnabled ? Math.round(ageValue * AGE_UNIT_SECONDS[ageUnit]) : null;
    const maxSizeBytes = sizeEnabled ? Math.round(sizeValue * SIZE_UNIT_BYTES[sizeUnit]) : null;
    const trimmedWebhook = webhookUrl.trim();
    const trimmedGroup = group.trim();
    const scheduleStartMinute = timeToMinutes(scheduleStartTime);
    const scheduleEndMinute = timeToMinutes(scheduleEndTime);

    try {
      validate_segment_seconds(segmentSeconds);
      validate_retention(
        recordingEnabled,
        maxAgeSecs != null ? BigInt(maxAgeSecs) : null,
        maxSizeBytes != null ? BigInt(maxSizeBytes) : null,
      );
      validate_sensitivity(motionSensitivity);
      if (trimmedWebhook) {
        validate_webhook_url(trimmedWebhook);
      }
      validate_group_name(trimmedGroup);
      validate_schedule_minute(scheduleStartMinute);
      validate_schedule_minute(scheduleEndMinute);
    } catch (e) {
      error = (e as Error).message;
      return;
    }

    saving = true;
    try {
      await updateCamera(camera.id, {
        recording: {
          enabled: recordingEnabled,
          trigger: recordingTrigger,
          segment_seconds: segmentSeconds,
          retention_max_age_secs: maxAgeSecs,
          retention_max_size_bytes: maxSizeBytes,
          schedule: {
            enabled: scheduleEnabled,
            days: scheduleDays,
            start_minute: scheduleStartMinute,
            end_minute: scheduleEndMinute,
          },
        },
        motion: {
          enabled: motionEnabled,
          sensitivity: motionSensitivity,
          webhook_url: trimmedWebhook || null,
        },
        rotation,
        overlay_timestamp: overlayTimestamp,
        group: trimmedGroup,
      });
      dispatch("updated");
      dispatch("close");
    } catch (e) {
      error = (e as Error).message;
    } finally {
      saving = false;
    }
  }
</script>

<svelte:window on:keydown={onKeydown} />

<div class="backdrop" role="presentation" on:click={() => dispatch("close")}>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Camera recording settings"
    on:click|stopPropagation
  >
    <h2>{camera.name} - Recording &amp; motion</h2>

    <label class="row">
      <input type="checkbox" bind:checked={recordingEnabled} disabled={!wasmReady} />
      Enable recording
    </label>

    {#if recordingEnabled}
      <label>
        Record
        <select bind:value={recordingTrigger} disabled={!wasmReady}>
          <option value="continuous">Continuously</option>
          <option value="motion">Only while motion is detected</option>
        </select>
      </label>
      {#if recordingTrigger === "motion" && !motionEnabled}
        <p class="hint">
          Motion detection will run automatically to gate recording, even though "Detect motion"
          below is off - that switch only controls event logging/webhooks.
        </p>
      {/if}

      <label>
        Segment length (minutes)
        <input
          type="number"
          min="0.2"
          max="60"
          step="0.5"
          bind:value={segmentMinutes}
          disabled={!wasmReady}
        />
      </label>

      <p class="hint">
        Recording loops forever, deleting the oldest segments once a limit below is hit. Set at
        least one.
      </p>

      <label class="row">
        <input type="checkbox" bind:checked={ageEnabled} disabled={!wasmReady} />
        Delete recordings older than
      </label>
      {#if ageEnabled}
        <div class="unit-row">
          <input type="number" min="1" bind:value={ageValue} disabled={!wasmReady} />
          <select bind:value={ageUnit} disabled={!wasmReady}>
            <option value="minutes">minutes</option>
            <option value="hours">hours</option>
            <option value="days">days</option>
            <option value="months">months</option>
          </select>
        </div>
      {/if}

      <label class="row">
        <input type="checkbox" bind:checked={sizeEnabled} disabled={!wasmReady} />
        Delete oldest once total size exceeds
      </label>
      {#if sizeEnabled}
        <div class="unit-row">
          <input type="number" min="1" bind:value={sizeValue} disabled={!wasmReady} />
          <select bind:value={sizeUnit} disabled={!wasmReady}>
            <option value="MB">MB</option>
            <option value="GB">GB</option>
            <option value="TB">TB</option>
          </select>
        </div>
      {/if}

      <label class="row">
        <input type="checkbox" bind:checked={scheduleEnabled} disabled={!wasmReady} />
        Only record during scheduled times
      </label>
      {#if scheduleEnabled}
        <div class="day-toggles">
          {#each DAY_LABELS as label, i (label)}
            <button
              type="button"
              class:active={scheduleDays[i]}
              on:click={() => (scheduleDays[i] = !scheduleDays[i])}
              disabled={!wasmReady}>{label}</button
            >
          {/each}
        </div>
        <div class="schedule-time-row">
          <label>
            From
            <input type="time" bind:value={scheduleStartTime} disabled={!wasmReady} />
          </label>
          <label>
            To
            <input type="time" bind:value={scheduleEndTime} disabled={!wasmReady} />
          </label>
        </div>
        <p class="hint">
          Recording only happens during this window on the checked days (an end time earlier than
          the start time spans past midnight). Live view isn't affected - the camera itself is
          still watchable any time.
        </p>
      {/if}
    {/if}

    <hr />

    <label>
      Rotation
      <select bind:value={rotation} disabled={!wasmReady}>
        <option value="none">None</option>
        <option value="clockwise90">90° clockwise</option>
        <option value="rotate180">180°</option>
        <option value="counter_clockwise90">90° counter-clockwise</option>
      </select>
    </label>
    <p class="hint">
      For a camera mounted sideways or upside down. Applied to the live view, recordings, and RTSP
      alike - takes effect after saving, disconnecting anyone currently watching this camera.
    </p>

    <label class="row">
      <input type="checkbox" bind:checked={overlayTimestamp} disabled={!wasmReady} />
      Show timestamp overlay
    </label>
    <p class="hint">
      Burns the current date/time onto the video itself (bottom-right corner), so it's in the live
      view, recordings, and RTSP alike - takes effect after saving, disconnecting anyone currently
      watching this camera.
    </p>

    <label>
      Group (optional)
      <input
        type="text"
        bind:value={group}
        placeholder="e.g. Front yard"
        disabled={!wasmReady}
      />
    </label>
    <p class="hint">Shown as a tab on the dashboard. Leave blank to keep this camera ungrouped.</p>

    <hr />

    <label class="row">
      <input type="checkbox" bind:checked={motionEnabled} disabled={!wasmReady} />
      Detect motion (log events &amp; webhook)
    </label>

    <label>
      Sensitivity ({motionSensitivity})
      <input
        type="range"
        min="1"
        max="100"
        bind:value={motionSensitivity}
        disabled={!wasmReady}
      />
    </label>

    <label>
      Webhook URL (optional)
      <input
        type="text"
        bind:value={webhookUrl}
        placeholder="https://example.com/hooks/motion"
        disabled={!wasmReady}
      />
    </label>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    <div class="actions">
      <button class="secondary" on:click={() => dispatch("close")}>Cancel</button>
      <button class="primary" on:click={save} disabled={saving || !wasmReady}>
        {saving ? "Saving…" : "Save"}
      </button>
    </div>
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
    padding: 1.5rem;
    width: min(380px, 100%);
    max-height: 90vh;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
  h2 {
    margin: 0 0 0.25rem;
    font-size: 1.05rem;
  }
  .hint {
    margin: 0;
    color: var(--text-dim);
    font-size: 0.78rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.8rem;
    color: var(--text-dim);
  }
  label.row {
    flex-direction: row;
    align-items: center;
    gap: 0.5rem;
    color: var(--text);
    font-size: 0.85rem;
  }
  input[type="number"],
  input[type="text"],
  select {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.45rem 0.6rem;
    color: var(--text);
    font-size: 0.9rem;
  }
  input[type="range"] {
    width: 100%;
  }
  hr {
    border: none;
    border-top: 1px solid var(--border);
    margin: 0.25rem 0;
  }
  .unit-row {
    display: flex;
    gap: 0.5rem;
    margin-left: 1.6rem;
  }
  .unit-row input {
    flex: 1;
  }
  .day-toggles {
    display: flex;
    gap: 0.3rem;
    margin-left: 1.6rem;
  }
  .day-toggles button {
    flex: 1;
    background: var(--bg);
    border: 1px solid var(--border);
    color: var(--text-dim);
    border-radius: 6px;
    padding: 0.3rem 0;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .day-toggles button.active {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }
  .schedule-time-row {
    display: flex;
    gap: 0.75rem;
    margin-left: 1.6rem;
  }
  .schedule-time-row label {
    flex: 1;
  }
  .schedule-time-row input {
    width: 100%;
  }
  .error {
    color: #e74c3c;
    font-size: 0.8rem;
    margin: 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.45rem 1rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .primary {
    background: var(--accent);
    color: #fff;
  }
  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .secondary {
    background: transparent;
    color: var(--text-dim);
    border: 1px solid var(--border);
  }
</style>
