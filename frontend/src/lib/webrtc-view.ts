// Shared WebRTC signaling (trickle ICE, see docs/ARCHITECTURE.md) for
// connecting a <video> element to a camera's live view - used by both
// CameraTile (the dashboard grid) and ExpandedCameraModal (the
// click-to-expand view), so this non-trivial, easy-to-get-subtly-wrong
// logic exists in exactly one place rather than two copies that could
// drift apart.
import { streamWsUrl } from "./api";

export type CameraViewStatus = "connecting" | "live" | "error";

// The server sends this exact message (see omni-server::supervisor's
// merge_end_signals) when it intentionally tore down a viewer's pipeline
// because the camera's settings changed - not a failure, so a viewer
// should reconnect automatically rather than show a hard error the user
// has to click through. Exported so both CameraTile and
// ExpandedCameraModal check for it the same way.
export const SETTINGS_CHANGED_MESSAGE = "camera settings changed; please reconnect";

export interface CameraViewHandlers {
  onStatusChange: (status: CameraViewStatus) => void;
  onError: (message: string) => void;
}

export interface CameraViewConnection {
  disconnect: () => void;
}

export function connectCameraView(
  cameraId: string,
  videoEl: HTMLVideoElement,
  handlers: CameraViewHandlers,
): CameraViewConnection {
  handlers.onStatusChange("connecting");

  let pc: RTCPeerConnection | null = new RTCPeerConnection({
    iceServers: [{ urls: "stun:stun.l.google.com:19302" }],
  });
  pc.addTransceiver("video", { direction: "recvonly" });

  let ws: WebSocket | null = new WebSocket(streamWsUrl(cameraId));

  pc.ontrack = (event) => {
    videoEl.srcObject = event.streams[0];
    handlers.onStatusChange("live");
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
      handlers.onStatusChange("error");
      handlers.onError("connection lost");
    }
  };

  ws.onmessage = async (event) => {
    const msg = JSON.parse(event.data);
    if (msg.type === "answer") {
      try {
        await pc?.setRemoteDescription({ type: "answer", sdp: msg.sdp });
      } catch (e) {
        handlers.onStatusChange("error");
        handlers.onError((e as Error).message);
      }
    } else if (msg.type === "ice_candidate") {
      try {
        await pc?.addIceCandidate(msg.candidate);
      } catch {
        // A late/duplicate candidate after the connection already
        // settled isn't worth surfacing as an error.
      }
    } else if (msg.type === "error") {
      handlers.onStatusChange("error");
      handlers.onError(msg.message);
    }
  };

  ws.onerror = () => {
    handlers.onStatusChange("error");
    handlers.onError("signaling connection failed");
  };

  ws.onopen = async () => {
    if (!pc) return;
    try {
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      ws?.send(JSON.stringify({ type: "offer", sdp: pc.localDescription?.sdp }));
    } catch (e) {
      handlers.onStatusChange("error");
      handlers.onError((e as Error).message);
    }
  };

  return {
    disconnect() {
      ws?.close();
      ws = null;
      pc?.close();
      pc = null;
    },
  };
}
