<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import init, { validate_password } from "./wasm/omni_wasm.js";
  import { changePassword } from "./api";

  const dispatch = createEventDispatcher();

  let currentPassword = "";
  let newPassword = "";
  let confirmPassword = "";
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
    if (newPassword !== confirmPassword) {
      error = "New passwords don't match";
      return;
    }
    try {
      validate_password(newPassword);
    } catch (e) {
      error = (e as Error).message;
      return;
    }
    saving = true;
    try {
      await changePassword(currentPassword, newPassword);
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
    aria-label="Change password"
    on:click|stopPropagation
  >
    <h2>Change password</h2>

    <label>
      Current password
      <input type="password" bind:value={currentPassword} disabled={!wasmReady} />
    </label>
    <label>
      New password
      <input type="password" bind:value={newPassword} disabled={!wasmReady} />
    </label>
    <label>
      Confirm new password
      <input type="password" bind:value={confirmPassword} disabled={!wasmReady} />
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
    width: 340px;
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
  h2 {
    margin: 0 0 0.25rem;
    font-size: 1.05rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.8rem;
    color: var(--text-dim);
  }
  input {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.45rem 0.6rem;
    color: var(--text);
    font-size: 0.9rem;
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
