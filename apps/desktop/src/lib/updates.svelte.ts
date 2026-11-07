/**
 * Update checks against GitHub Releases (signed with the project's updater key).
 * At most once a day, only when "Check for updates" is on, and never in a plain browser.
 * The only request made is for the release manifest; nothing about the user is sent.
 */
import { invoke } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { api, isTauri } from "./api";

const CHECK_KEY = "check_updates";
const LAST_KEY = "update_checked_at";
const DAY = 24 * 60 * 60;

class Updates {
  available = $state<{ version: string; notes: string } | null>(null);
  /** 0..1 while downloading, null otherwise. */
  progress = $state<number | null>(null);
  error = $state<string | null>(null);
  lastResult = $state<string | null>(null);
  private update: Update | null = null;

  async enabled(): Promise<boolean> {
    return (await api.getSetting(CHECK_KEY)) !== "false";
  }

  async setEnabled(on: boolean) {
    await api.setSetting(CHECK_KEY, String(on));
  }

  /** Called on startup: checks if enabled and the last check is over a day old. */
  async maybeCheck() {
    if (!isTauri() || import.meta.env.DEV) return;
    if (!(await this.enabled())) return;
    const last = Number((await api.getSetting(LAST_KEY)) ?? 0);
    if (Date.now() / 1000 - last < DAY) return;
    await this.checkNow(true);
  }

  async checkNow(quiet = false): Promise<void> {
    this.error = null;
    try {
      const u = await check({ timeout: 15000 });
      await api.setSetting(LAST_KEY, String(Math.floor(Date.now() / 1000)));
      this.update = u;
      if (u) {
        this.available = { version: u.version, notes: u.body ?? "" };
        this.lastResult = `Storecat ${u.version} is available.`;
      } else {
        this.available = null;
        this.lastResult = "You have the latest version.";
      }
    } catch (e) {
      // No published release yet, offline, or GitHub unreachable: say so only when asked.
      this.lastResult = quiet ? null : "Couldn't reach the update server.";
      if (!quiet) this.error = String(e);
    }
  }

  async install() {
    if (!this.update) return;
    let total = 0;
    let done = 0;
    this.progress = 0;
    try {
      await this.update.downloadAndInstall((ev) => {
        if (ev.event === "Started") total = ev.data.contentLength ?? 0;
        if (ev.event === "Progress") {
          done += ev.data.chunkLength;
          this.progress = total ? done / total : null;
        }
        if (ev.event === "Finished") this.progress = 1;
      });
      await relaunch();
    } catch (e) {
      this.progress = null;
      this.error = String(e);
    }
  }

  dismiss() {
    this.available = null;
  }

  releaseNotes(version: string) {
    return invoke("open_release_notes", { version });
  }
}

export const updates = new Updates();
