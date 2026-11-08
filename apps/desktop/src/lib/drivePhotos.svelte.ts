/**
 * Which drives have a photo, for showing it when a drive's name is hovered anywhere in
 * the app. Kept in step with the volume list by the store.
 */
import type { Volume } from "./api";

/** Drive id → a token that changes when the photo might have. */
export const drivePhotos: Record<number, string> = $state({});

export function syncDrivePhotos(volumes: Volume[]) {
  const live = new Set<number>();
  for (const v of volumes) {
    if (!v.photo) continue;
    live.add(v.id);
    const token = String(v.last_seen_at ?? 0);
    if (drivePhotos[v.id] !== token) drivePhotos[v.id] = token;
  }
  for (const id of Object.keys(drivePhotos).map(Number)) {
    if (!live.has(id)) delete drivePhotos[id];
  }
}
