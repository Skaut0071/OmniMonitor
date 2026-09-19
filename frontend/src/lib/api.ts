export type RecordingTrigger = "continuous" | "motion";

export interface RecordingSettings {
  enabled: boolean;
  trigger: RecordingTrigger;
  segment_seconds: number;
  retention_max_age_secs: number | null;
  retention_max_size_bytes: number | null;
}

export interface MotionSettings {
  enabled: boolean;
  sensitivity: number;
  webhook_url: string | null;
}

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
  recording: RecordingSettings;
  motion: MotionSettings;
  status?: "idle" | "streaming" | "error";
}

export interface RecordingInfo {
  filename: string;
  size_bytes: number;
  modified: string;
}

export interface MotionEvent {
  id: string;
  camera_id: string;
  started_at: string;
  ended_at: string | null;
}

export interface UpdateCameraRequest {
  name?: string;
  url?: string;
  width?: number;
  height?: number;
  framerate?: number;
  recording?: RecordingSettings;
  motion?: MotionSettings;
}

const BASE = "/api";

async function unwrap<T>(res: Response, fallback: string): Promise<T> {
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `${fallback}: ${res.status}`);
  }
  return res.json();
}

export async function listCameras(): Promise<Camera[]> {
  return unwrap(await fetch(`${BASE}/cameras`), "failed to list cameras");
}

export async function discoverCameras(): Promise<Camera[]> {
  return unwrap(
    await fetch(`${BASE}/cameras/discover`, { method: "POST" }),
    "failed to discover cameras",
  );
}

export interface OnvifDevice {
  address: string;
  xaddrs: string[];
}

// Probes the LAN for ONVIF cameras (WS-Discovery multicast) - takes a
// few seconds since it waits out a fixed collection window server-side.
export async function discoverOnvifDevices(): Promise<OnvifDevice[]> {
  return unwrap(
    await fetch(`${BASE}/onvif/discover`, { method: "POST" }),
    "failed to scan for network cameras",
  );
}

export async function createRtspCamera(name: string, url: string): Promise<Camera> {
  return unwrap(
    await fetch(`${BASE}/cameras`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, url }),
    }),
    "failed to create camera",
  );
}

export async function updateCamera(id: string, patch: UpdateCameraRequest): Promise<Camera> {
  return unwrap(
    await fetch(`${BASE}/cameras/${id}`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    }),
    "failed to update camera",
  );
}

export async function deleteCamera(id: string): Promise<void> {
  const res = await fetch(`${BASE}/cameras/${id}`, { method: "DELETE" });
  if (!res.ok && res.status !== 204) {
    throw new Error(`failed to delete camera: ${res.status}`);
  }
}

export async function listRecordings(cameraId: string): Promise<RecordingInfo[]> {
  return unwrap(
    await fetch(`${BASE}/cameras/${cameraId}/recordings`),
    "failed to list recordings",
  );
}

export async function deleteRecording(cameraId: string, filename: string): Promise<void> {
  const res = await fetch(`${BASE}/recordings/${cameraId}/${encodeURIComponent(filename)}`, {
    method: "DELETE",
  });
  if (!res.ok && res.status !== 204) {
    throw new Error(`failed to delete recording: ${res.status}`);
  }
}

export function recordingUrl(cameraId: string, filename: string): string {
  return `${BASE}/recordings/${cameraId}/${encodeURIComponent(filename)}`;
}

export async function getMotionStatus(cameraId: string): Promise<boolean> {
  const data = await unwrap<{ active: boolean }>(
    await fetch(`${BASE}/cameras/${cameraId}/motion`),
    "failed to get motion status",
  );
  return data.active;
}

export async function listMotionEvents(cameraId: string): Promise<MotionEvent[]> {
  return unwrap(await fetch(`${BASE}/cameras/${cameraId}/events`), "failed to list events");
}

export function streamWsUrl(cameraId: string): string {
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${window.location.host}/api/stream/${cameraId}`;
}

export interface Me {
  username: string;
}

/** Returns the logged-in username, or null if there's no valid session. */
export async function me(): Promise<Me | null> {
  const res = await fetch(`${BASE}/auth/me`);
  if (res.status === 401) return null;
  return unwrap(res, "failed to check session");
}

export async function login(username: string, password: string): Promise<void> {
  const res = await fetch(`${BASE}/auth/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `login failed: ${res.status}`);
  }
}

export async function logout(): Promise<void> {
  await fetch(`${BASE}/auth/logout`, { method: "POST" });
}

export async function changePassword(currentPassword: string, newPassword: string): Promise<void> {
  const res = await fetch(`${BASE}/auth/change-password`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `failed to change password: ${res.status}`);
  }
}

export interface RtspCredentials {
  username: string;
  password: string;
  port: number;
}

export async function getRtspCredentials(): Promise<RtspCredentials> {
  const res = await fetch(`${BASE}/rtsp-credentials`);
  return unwrap(res, "failed to load RTSP credentials");
}
