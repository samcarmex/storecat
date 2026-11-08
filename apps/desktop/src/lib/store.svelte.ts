/**
 * App-wide reactive state: volumes, catalogue stats, live scan and enrichment progress.
 * One instance, created in App.svelte, refreshed from backend events.
 */
import {
  api,
  type CatalogStats,
  type DetectedVolume,
  type DownloadProgress,
  type EnrichCoverage,
  type EnrichProgress,
  type ScanProgress,
  type VerifyProgress,
  type Volume,
} from "./api";

export class AppStore {
  volumes = $state<Volume[]>([]);
  detected = $state<DetectedVolume[]>([]);
  stats = $state<CatalogStats>({
    index_rebuilding: false, volumes: 0, online: 0, files: 0, dirs: 0, bytes: 0, capacity: 0, free_online: 0,
  });
  /** volume id → latest scan progress while a scan runs */
  progress = $state<Record<number, ScanProgress>>({});
  /** volume id → latest enrichment progress while it runs */
  enrich = $state<Record<number, EnrichProgress>>({});
  /** volume ids whose enrichment was asked to pause and is finishing the files in hand */
  pausing = $state<Record<number, true>>({});
  /** volume id → enrichment coverage, refreshed with the volume list */
  coverage = $state<Record<number, EnrichCoverage>>({});
  download = $state<DownloadProgress | null>(null);
  /** Duplicate groups being hashed in full, by quick hash. Kept here so the Duplicates view
   * shows them again after switching away and back. */
  verifying = $state<Record<string, VerifyProgress>>({});
  /** Incremented whenever the catalogue content changed, so the search view re-runs. */
  catalogVersion = $state(0);
  error = $state<string | null>(null);

  readonly online = $derived(this.volumes.filter((v) => v.online).length);

  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  private rebuildTimer: ReturnType<typeof setTimeout> | null = null;
  private bumpTimer: ReturnType<typeof setTimeout> | null = null;
  private liveTimer: ReturnType<typeof setTimeout> | null = null;

  async init() {
    await this.refresh();
    await api.onVolumesChanged(() => this.scheduleRefresh());
    await api.onCatalogChanged(() => this.bumpCatalog());
    for (const p of await api.verifying()) this.verifying[p.hash] = p;
    await api.onVerifyProgress((p) => {
      if (p.done) {
        delete this.verifying[p.hash];
        this.catalogVersion += 1;
      } else {
        this.verifying[p.hash] = p;
      }
    });
    await api.onScanProgress((p) => this.onScan(p));
    await api.onEnrichProgress((p) => this.onEnrich(p));
    await api.onToolDownload((p) => {
      this.download = p.done ? null : p;
      if (p.done && p.error) this.error = p.error;
    });
  }

  /** Live changes arrive every few seconds while files change: re-run views at most once a second. */
  private bumpCatalog() {
    if (this.liveTimer) return;
    this.liveTimer = setTimeout(() => {
      this.liveTimer = null;
      this.catalogVersion += 1;
    }, 1000);
  }

  scheduleRefresh() {
    if (this.refreshTimer) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      void this.refresh();
    }, 80);
  }

  async refresh() {
    try {
      const [volumes, detected, stats, scanning, enriching, queued] = await Promise.all([
        api.listVolumes(),
        api.listDetected(),
        api.catalogStats(),
        api.scanning(),
        api.enriching(),
        api.enrichQueued(),
      ]);
      this.volumes = volumes;
      this.detected = detected;
      this.stats = stats;
      this.watchRebuild();
      for (const id of scanning) {
        if (!this.progress[id]) {
          this.progress[id] = {
            phase: "walking", volume_id: id, files: 0, dirs: 0, bytes: 0, errors: 0, elapsed_ms: 0, current: "",
          };
        }
      }
      for (const id of enriching) {
        if (this.enrich[id]?.phase !== "running") this.enrich[id] = blankEnrich(id, "running");
      }
      for (const id of queued) {
        if (!this.enrich[id]) this.enrich[id] = blankEnrich(id, "queued");
      }
      const cov = await Promise.all(volumes.map((v) => api.enrichCoverage(v.id).catch(() => null)));
      const next: Record<number, EnrichCoverage> = {};
      volumes.forEach((v, i) => {
        if (cov[i]) next[v.id] = cov[i]!;
      });
      this.coverage = next;
      this.error = null;
    } catch (e) {
      this.error = String(e);
    }
  }

  /** While the search index rebuilds in the background, poll its progress until it's done. */
  private watchRebuild() {
    if (!this.stats.index_rebuilding || this.rebuildTimer) return;
    this.rebuildTimer = setTimeout(async () => {
      this.rebuildTimer = null;
      try {
        const s = await api.catalogStats();
        const finished = this.stats.index_rebuilding && !s.index_rebuilding;
        this.stats = s;
        if (finished) this.catalogVersion += 1;
      } catch {
        // try again on the next refresh
      }
      this.watchRebuild();
    }, 1500);
  }

  private onScan(p: ScanProgress) {
    if (p.phase === "done" || p.phase === "failed") {
      this.progress[p.volume_id] = p;
      this.catalogVersion += 1;
      setTimeout(() => {
        if (this.progress[p.volume_id]?.phase === p.phase) delete this.progress[p.volume_id];
      }, p.phase === "failed" ? 8000 : 2500);
    } else {
      this.progress[p.volume_id] = p;
    }
  }

  private onEnrich(p: EnrichProgress) {
    if (p.phase === "queued") {
      this.enrich[p.volume_id] = p;
    } else if (p.phase === "running") {
      this.enrich[p.volume_id] = p;
      // Results grow while enrichment runs; nudge the search view now and then.
      if (!this.bumpTimer) {
        this.bumpTimer = setTimeout(() => {
          this.bumpTimer = null;
          this.catalogVersion += 1;
        }, 4000);
      }
    } else {
      this.enrich[p.volume_id] = p;
      delete this.pausing[p.volume_id];
      this.catalogVersion += 1;
      // Stopped for read errors: the drive now carries a fault to show.
      if (p.phase === "stopped") void this.refresh();
      setTimeout(() => {
        if (this.enrich[p.volume_id]?.phase === p.phase) delete this.enrich[p.volume_id];
      }, p.phase === "failed" || p.phase === "stopped" ? 10000 : 3000);
    }
  }

  isScanning(id: number): boolean {
    const p = this.progress[id];
    return !!p && (p.phase === "walking" || p.phase === "committing");
  }

  /** Enriching now, or queued behind another drive. */
  isEnriching(id: number): boolean {
    const phase = this.enrich[id]?.phase;
    return phase === "running" || phase === "queued";
  }

  async startScan(id: number) {
    try {
      this.progress[id] = {
        phase: "walking", volume_id: id, files: 0, dirs: 0, bytes: 0, errors: 0, elapsed_ms: 0, current: "",
      };
      await api.startScan(id);
    } catch (e) {
      delete this.progress[id];
      this.error = String(e);
    }
  }

  async startEnrich(id: number) {
    try {
      // Queued until the backend says it has started (straight away if nothing else runs).
      this.enrich[id] = blankEnrich(id, Object.values(this.enrich).some((e) => e.phase === "running") ? "queued" : "running");
      await api.startEnrich(id);
    } catch (e) {
      delete this.enrich[id];
      this.error = String(e);
    }
  }

  /** Throw away a drive's enrichment (hashes kept) and run it again, e.g. to pick up new kinds of thumbnails. */
  async redoEnrichment(id: number) {
    try {
      await api.resetEnrichment(id, true);
      await this.refresh();
      await this.startEnrich(id);
    } catch (e) {
      this.error = String(e);
    }
  }

  async cancelEnrich(id: number) {
    // A queued drive drops out at once; a running one finishes the files it is reading
    // (a slow video can take a while), so say the pause was heard.
    if (this.enrich[id]?.phase === "running") this.pausing[id] = true;
    try {
      await api.cancelEnrich(id);
    } catch (e) {
      delete this.pausing[id];
      this.error = String(e);
    }
  }

  async addDetected(identity: string, scan = true) {
    try {
      const v = await api.addDetected(identity);
      await this.refresh();
      if (scan) await this.startScan(v.id);
    } catch (e) {
      this.error = String(e);
    }
  }

  async pickFolder() {
    try {
      const v = await api.pickAndAddFolder();
      if (v) {
        await this.refresh();
        await this.startScan(v.id);
      }
    } catch (e) {
      this.error = String(e);
    }
  }

  async importCatalogue() {
    try {
      const v = await api.importVolume();
      if (v) {
        this.catalogVersion += 1;
        await this.refresh();
      }
    } catch (e) {
      this.error = String(e);
    }
  }

  async exportVolume(id: number): Promise<string | null> {
    try {
      return await api.exportVolume(id);
    } catch (e) {
      this.error = String(e);
      return null;
    }
  }

  async removeVolume(id: number) {
    try {
      await api.removeVolume(id);
      delete this.progress[id];
      delete this.enrich[id];
      this.catalogVersion += 1;
      await this.refresh();
    } catch (e) {
      this.error = String(e);
    }
  }

  async updateVolume(id: number, patch: Parameters<typeof api.updateVolume>[1]) {
    try {
      await api.updateVolume(id, patch);
      await this.refresh();
    } catch (e) {
      this.error = String(e);
    }
  }
}

function blankEnrich(id: number, phase: "queued" | "running"): EnrichProgress {
  return {
    phase, volume_id: id, pass: 0, done: 0, total: 0, hashed: 0, metas: 0, thumbs: 0, texts: 0, archives: 0,
    elapsed_ms: 0, current: "",
  };
}
