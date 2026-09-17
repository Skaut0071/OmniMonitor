<script lang="ts">
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import type { Camera } from "./api";
  import { getMotionStatus, streamWsUrl } from "./api";

  export let camera: Camera;

  const dispatch = createEventDispatcher();

  let videoEl: HTMLVideoElement;
  let status: "connecting" | "live" | "error" | "idle" = "idle";
  let errorMessage = "";
  let motionActive = false;

  let pc: RTCPeerConnection | null = null;
  let ws: WebSocket | null = null;
  let motionPoll: ReturnType<typeof setInterval> | null = null;

  async function connect() {
    status = "connecting";
    errorMessage = "";

    pc = new RTCPeerConnection({
      iceServers: [{ urls: "stun:stun.l.google.com:19302" }],
    });
    pc.addTransceiver("video", { direction: "recvonly" });

    pc.ontrack = (event) => {
      if (videoEl) {
        videoEl.srcObject = event.streams[0];
      }
      status = "live";
    };

    pc.onicecandidate = (event) => {
      if (event.candidate && ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "ice_candidate", candidate: event.candidate.toJSON() }));
      }
    };

    pc.onconnectionstatechange = () => {
      if (!pc) return;
      if (
        pc.connectionState === "failed" ||
        pc.connectionState === "disconnected" ||
        pc.connectionState === "closed"
      ) {
        status = "error";
        errorMessage = errorMessage || "connection lost";
      }
    };

    ws = new WebSocket(streamWsUrl(camera.id));

    ws.onmessage = async (event) => {
      const msg = JSON.parse(event.data);
      if (msg.type === "answer") {
        try {
          await pc?.setRemoteDescription({ type: "answer", sdp: msg.sdp });
        } catch (e) {
          status = "error";
          errorMessage = (e as Error).message;
        }
      } else if (msg.type === "ice_candidate") {
        try {
          await pc?.addIceCandidate(msg.candidate);
        } catch {
          // A late/duplicate candidate after the connection already
          // settled isn't worth surfacing as a tile error.
        }
      } else if (msg.type === "error") {
        status = "error";
        errorMessage = msg.message;
      }
    };

    ws.onerror = () => {
      status = "error";
      errorMessage = errorMessage || "signaling connection failed";
    };

    ws.onopen = async () => {
      if (!pc) return;
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        ws?.send(JSON.stringify({ type: "offer", sdp: pc.localDescription?.sdp }));
      } catch (e) {
        status = "error";
        errorMessage = (e as Error).message;
      }
    };
  }

  function disconnect() {
    ws?.close();
    ws = null;
    pc?.close();
    pc = null;
  }

  function retry() {
    disconnect();
    connect();
  }

  onMount(() => {
    connect();
    if (camera.motion.enabled || (camera.recording.enabled && camera.recording.trigger === "motion")) {
      const poll = async () => {
        try {
          motionActive = await getMotionStatus(camera.id);
        } catch {
          // Transient poll failures aren't worth surfacing as tile errors.
        }
      };
      poll();
      motionPoll = setInterval(poll, 3000);
    }
  });

  onDestroy(() => {
    disconnect();
    if (motionPoll) clearInterval(motionPoll);
  });
</script>

<div class="tile">
  <div class="tile-header">
    <span class="name">{camera.name}</span>
    <div class="badges">
      {#if motionActive}
        <span class="motion-badge" title="Motion detected">MOTION</span>
      {/if}
      {#if camera.recording.enabled}
        <span class="rec-badge" title="Recording">● REC</span>
      {/if}
      <span class="status status-{status}">{status}</span>
      <button class="icon-btn" title="Recordings" on:click={() => dispatch("recordings")}
        >⏺</button
      >
      <button class="icon-btn" title="Settings" on:click={() => dispatch("settings")}>⚙</button>
      <button class="icon-btn" title="Remove camera" on:click={() => dispatch("remove")}
        >✕</button
      >
    </div>
  </div>
  <div class="video-wrap">
    <!-- svelte-ignore a11y-media-has-caption -->
    <video bind:this={videoEl} autoplay playsinline muted></video>
    {#if status !== "live"}
      <div class="overlay">
        {#if status === "error"}
          <span>⚠ {errorMessage || "stream error"}</span>
          <button on:click={retry}>Retry</button>
        {:else}
          <span>Connecting…</span>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .tile {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .tile-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0.75rem;
    font-size: 0.85rem;
  }
  .name {
    font-weight: 600;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badges {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-shrink: 0;
  }
  .rec-badge {
    font-size: 0.65rem;
    font-weight: 700;
    letter-spacing: 0.03em;
    color: #e74c3c;
  }
  .motion-badge {
    font-size: 0.65rem;
    font-weight: 700;
    letter-spacing: 0.03em;
    color: #f1c40f;
    background: rgba(241, 196, 15, 0.15);
    padding: 0.1rem 0.4rem;
    border-radius: 999px;
  }
  .icon-btn {
    background: transparent;
    border: none;
    color: var(--text-dim);
    cursor: pointer;
    font-size: 0.75rem;
    padding: 0.1rem 0.25rem;
    line-height: 1;
  }
  .icon-btn:hover {
    color: var(--text);
  }
  .status {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0.15rem 0.5rem;
    border-radius: 999px;
  }
  .status-live {
    background: rgba(46, 204, 113, 0.15);
    color: #2ecc71;
  }
  .status-connecting {
    background: rgba(241, 196, 15, 0.15);
    color: #f1c40f;
  }
  .status-error {
    background: rgba(231, 76, 60, 0.15);
    color: #e74c3c;
  }
  .status-idle {
    background: rgba(149, 165, 166, 0.15);
    color: #95a5a6;
  }
  .video-wrap {
    position: relative;
    background: #000;
    aspect-ratio: 16 / 9;
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
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    color: var(--text-dim);
    font-size: 0.85rem;
    background: rgba(0, 0, 0, 0.35);
  }
  .overlay button {
    background: var(--accent);
    color: #fff;
    border: none;
    padding: 0.3rem 0.9rem;
    border-radius: 6px;
    cursor: pointer;
    font-size: 0.8rem;
  }
</style>
