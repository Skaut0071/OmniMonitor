<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { login } from "./api";

  const dispatch = createEventDispatcher();

  let username = "admin";
  let password = "";
  let error = "";
  let submitting = false;

  async function submit() {
    error = "";
    submitting = true;
    try {
      await login(username, password);
      dispatch("loggedIn");
    } catch (e) {
      error = (e as Error).message;
    } finally {
      submitting = false;
    }
  }
</script>

<div class="wrap">
  <form class="card" on:submit|preventDefault={submit}>
    <div class="brand">
      <span class="brand-mark">●</span>
      <span>OmniMonitor</span>
    </div>
    <label>
      Username
      <input type="text" bind:value={username} autocomplete="username" />
    </label>
    <label>
      Password
      <input type="password" bind:value={password} autocomplete="current-password" />
    </label>
    {#if error}
      <p class="error">{error}</p>
    {/if}
    <button class="primary" type="submit" disabled={submitting}>
      {submitting ? "Signing in…" : "Sign in"}
    </button>
  </form>
</div>

<style>
  .wrap {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 2rem;
    width: 320px;
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 700;
    font-size: 1.15rem;
    margin-bottom: 0.5rem;
  }
  .brand-mark {
    color: var(--accent);
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
    padding: 0.5rem 0.6rem;
    color: var(--text);
    font-size: 0.9rem;
  }
  .error {
    color: #e74c3c;
    font-size: 0.8rem;
    margin: 0;
  }
  button {
    border: none;
    border-radius: 6px;
    padding: 0.55rem 1rem;
    font-size: 0.9rem;
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
</style>
