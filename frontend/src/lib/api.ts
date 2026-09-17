export interface Camera {
  id: string;
  name: string;
  kind: "usb" | "rtsp";
  device_path?: string;
  url?: string;
  enabled: boolean;
  width: number;
  height: number;
  framerate: number;
  codec: "vp8" | "h264";
  status?: "idle" | "streaming" | "error";
}

const BASE = "/api";

export async function listCameras(): Promise<Camera[]> {
  const res = await fetch(`${BASE}/cameras`);
  if (!res.ok) throw new Error(`failed to list cameras: ${res.status}`);
  return res.json();
}

export async function discoverCameras(): Promise<Camera[]> {
  const res = await fetch(`${BASE}/cameras/discover`, { method: "POST" });
  if (!res.ok) throw new Error(`failed to discover cameras: ${res.status}`);
  return res.json();
}

export async function createRtspCamera(name: string, url: string): Promise<Camera> {
  const res = await fetch(`${BASE}/cameras`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, url }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `failed to create camera: ${res.status}`);
  }
  return res.json();
}

export async function deleteCamera(id: string): Promise<void> {
  const res = await fetch(`${BASE}/cameras/${id}`, { method: "DELETE" });
  if (!res.ok && res.status !== 204) {
    throw new Error(`failed to delete camera: ${res.status}`);
  }
}

export function streamWsUrl(cameraId: string): string {
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${window.location.host}/api/stream/${cameraId}`;
}
