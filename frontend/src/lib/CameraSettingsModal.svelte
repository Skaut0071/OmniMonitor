<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import init, {
    validate_retention,
    validate_segment_seconds,
    validate_sensitivity,
    validate_webhook_url,
  } from "./wasm/omni_wasm.js";
  import { updateCamera, type Camera, type RecordingTrigger } from "./api";

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
        },
        motion: {
          enabled: motionEnabled,
          sensitivity: motionSensitivity,
          webhook_url: trimmedWebhook || null,
        },
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
    {/if}

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
  }
  .modal {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.5rem;
    width: 380px;
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
