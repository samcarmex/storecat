<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { updates } from "../updates.svelte";
  import { api, ops, storage, type BuildInfo, type FfmpegStatus, type RuleSettings, type ShellStatus } from "../api";
  import type { AppStore } from "../store.svelte";
  import { bytes, count } from "../format";

  let { store }: { store: AppStore } = $props();

  let info = $state<BuildInfo | null>(null);
  let ffmpeg = $state<FfmpegStatus | null>(null);
  let autoRescan = $state(true);
  let autoEnrich = $state(true);
  let copyToDrive = $state(true);
  let hashMin = $state("1048576");
  let downloading = $state(false);
  let msg = $state<string | null>(null);
  let shell = $state<ShellStatus>({ tray_mode: false, hotkey_enabled: false, hotkey: "Ctrl+Shift+Space", autostart: false });

  async function setTray(on: boolean) {
    try {
      await ops.setTrayMode(on);
      shell.tray_mode = on;
      if (!on && shell.autostart) {
        await ops.setAutostart(false);
        shell.autostart = false;
      }
      msg = "Saved";
      setTimeout(() => (msg = null), 1200);
    } catch (e) {
      store.error = String(e);
    }
  }
  async function setHotkey(on: boolean) {
    try {
      await ops.setHotkeyEnabled(on);
      shell.hotkey_enabled = on;
    } catch (e) {
      store.error = String(e);
      shell.hotkey_enabled = !on;
    }
  }
  async function setAutostart(on: boolean) {
    try {
      shell.autostart = await ops.setAutostart(on);
    } catch (e) {
      store.error = String(e);
    }
  }

  const HASH_CHOICES = [
    ["65536", "64 KB"],
    ["262144", "256 KB"],
    ["1048576", "1 MB (default)"],
    ["10485760", "10 MB"],
    ["104857600", "100 MB"],
  ];

  onMount(async () => {
    info = await api.buildInfo();
    ffmpeg = await api.ffmpegStatus();
    autoRescan = (await api.getSetting("auto_rescan")) !== "false";
    autoEnrich = (await api.getSetting("auto_enrich")) !== "false";
    copyToDrive = (await api.getSetting("copy_catalog_to_drive")) !== "false";
    hashMin = (await api.getSetting("hash_min_bytes")) ?? "1048576";
    shell = await ops.shellStatus();
    rules = await storage.ruleSettings();
    checkUpdates = await updates.enabled();
    try {
      diag = await invoke<Diagnostics>("diagnostics_info");
    } catch {
      diag = null;
    }
  });

  interface Diagnostics {
    logs_dir: string;
    crash_reports: string[];
    verbose: boolean;
  }
  let diag = $state<Diagnostics | null>(null);
  let checkUpdates = $state(true);
  let checking = $state(false);
  async function checkNow() {
    checking = true;
    await updates.checkNow(false);
    checking = false;
  }

  let rules = $state<RuleSettings | null>(null);
  async function saveRules() {
    if (!rules) return;
    try {
      await storage.setRuleSettings($state.snapshot(rules));
      msg = "Saved";
      setTimeout(() => (msg = null), 1200);
    } catch (e) {
      store.error = String(e);
    }
  }
  async function resetSuggestions() {
    await storage.reset();
    msg = "Dismissed suggestions are back";
    setTimeout(() => (msg = null), 2000);
  }

  async function set(key: string, value: string) {
    try {
      await api.setSetting(key, value);
      msg = "Saved";
      setTimeout(() => (msg = null), 1200);
    } catch (e) {
      store.error = String(e);
    }
  }

  async function download() {
    downloading = true;
    try {
      ffmpeg = await api.downloadFfmpeg();
    } catch (e) {
      store.error = String(e);
    } finally {
      downloading = false;
    }
  }

  const dl = $derived(store.download);
</script>

<div class="head">
  <h1>Settings</h1>
  {#if msg}<span class="muted">{msg}</span>{/if}
</div>

<div class="grid">
  <section class="card">
    <h2>Behaviour</h2>
    <label class="toggle">
      <input type="checkbox" bind:checked={autoRescan} onchange={() => set("auto_rescan", String(autoRescan))} />
      <span>
        <strong>Rescan drives when they are plugged in</strong>
        <span class="muted">Silent, in the background. Skipped if the last scan was under 10 minutes ago.</span>
      </span>
    </label>
    <label class="toggle">
      <input type="checkbox" bind:checked={autoEnrich} onchange={() => set("auto_enrich", String(autoEnrich))} />
      <span>
        <strong>Enrich after scanning</strong>
        <span class="muted">Hashes, metadata, thumbnails and document text while the drive stays connected. Resumes where it stopped.</span>
      </span>
    </label>
    <label class="toggle">
      <input type="checkbox" bind:checked={copyToDrive} onchange={() => set("copy_catalog_to_drive", String(copyToDrive))} />
      <span>
        <strong>Keep a copy of the catalogue on the drive</strong>
        <span class="muted">Written to <code>.storecat\catalog.db</code> on the drive after each scan. Import it on another computer.</span>
      </span>
    </label>
    <label class="field">
      <span>
        <strong>Hash files larger than</strong>
        <span class="muted">Smaller files are not hashed or checked for duplicates. Takes effect on the next enrichment.</span>
      </span>
      <select bind:value={hashMin} onchange={() => set("hash_min_bytes", hashMin)}>
        {#each HASH_CHOICES as [v, l] (v)}
          <option value={v}>{l}</option>
        {/each}
      </select>
    </label>
  </section>

  <section class="card">
    <h2>Always at hand</h2>
    <label class="toggle">
      <input type="checkbox" checked={shell.tray_mode} onchange={(e) => setTray((e.currentTarget as HTMLInputElement).checked)} />
      <span>
        <strong>Keep running in the system tray</strong>
        <span class="muted">Closing the window hides it instead of quitting. Drives are watched and enriched in the background.</span>
      </span>
    </label>
    <label class="toggle">
      <input type="checkbox" checked={shell.hotkey_enabled} onchange={(e) => setHotkey((e.currentTarget as HTMLInputElement).checked)} />
      <span>
        <strong>Global shortcut <kbd>{shell.hotkey}</kbd></strong>
        <span class="muted">Pops a quick-search bar over whatever you are doing. Enter opens the file; Shift+Enter opens the search in Storecat.</span>
        <span><button class="btn small" onclick={() => ops.togglePalette()}>Try it now</button></span>
      </span>
    </label>
    <label class="toggle">
      <input type="checkbox" checked={shell.autostart} disabled={!shell.tray_mode} onchange={(e) => setAutostart((e.currentTarget as HTMLInputElement).checked)} />
      <span>
        <strong>Start with Windows</strong>
        <span class="muted">Starts hidden in the tray at sign-in. Needs tray mode.</span>
      </span>
    </label>
  </section>

  <section class="card">
    <h2>Suggestions</h2>
    {#if rules}
      <label class="toggle">
        <input type="checkbox" bind:checked={rules.enabled} onchange={saveRules} />
        <span>
          <strong>Suggest what to move, clean up and back up</strong>
          <span class="muted">Shown in Storage. Nothing runs until you review and start it.</span>
        </span>
      </label>
      <label class="field">
        <span>
          <strong>Call a folder cold after</strong>
          <span class="muted">Folders on SSDs with no changes for this long are suggested for an Archive drive.</span>
        </span>
        <select bind:value={rules.cold_days} onchange={saveRules} disabled={!rules.enabled}>
          <option value={180}>6 months</option>
          <option value={365}>1 year</option>
          <option value={730}>2 years</option>
          <option value={1825}>5 years</option>
        </select>
      </label>
      <label class="field">
        <span>
          <strong>Only for folders larger than</strong>
        </span>
        <select bind:value={rules.cold_min_gb} onchange={saveRules} disabled={!rules.enabled}>
          <option value={0.5}>500 MB</option>
          <option value={1}>1 GB</option>
          <option value={5}>5 GB</option>
          <option value={20}>20 GB</option>
        </select>
      </label>
      <label class="field">
        <span>
          <strong>Desktop + Downloads sweep every</strong>
        </span>
        <select bind:value={rules.sweep_days} onchange={saveRules} disabled={!rules.enabled}>
          <option value={30}>Month</option>
          <option value={90}>Quarter</option>
          <option value={182}>Six months</option>
          <option value={365}>Year</option>
        </select>
      </label>
      <label class="field">
        <span>
          <strong>Warn when a drive is fuller than</strong>
        </span>
        <select bind:value={rules.full_percent} onchange={saveRules} disabled={!rules.enabled}>
          <option value={80}>80%</option>
          <option value={85}>85%</option>
          <option value={90}>90%</option>
          <option value={95}>95%</option>
        </select>
      </label>
      <div><button class="btn small" onclick={resetSuggestions}>Bring back dismissed suggestions</button></div>
    {/if}
  </section>

  <section class="card">
    <h2>Updates and diagnostics</h2>
    <label class="toggle">
      <input type="checkbox" bind:checked={checkUpdates} onchange={() => updates.setEnabled(checkUpdates)} />
      <span>
        <strong>Check for updates once a day</strong>
        <span class="muted">Asks GitHub for the latest release manifest. Updates are signed and only install when you click Install.</span>
        <span class="inline">
          <button class="btn small" onclick={checkNow} disabled={checking}>{checking ? "Checking…" : "Check now"}</button>
          {#if updates.lastResult}<span class="muted">{updates.lastResult}</span>{/if}
        </span>
      </span>
    </label>
    {#if diag}
      <label class="toggle">
        <input
          type="checkbox"
          checked={diag.verbose}
          onchange={(e) => invoke("set_verbose_log", { enabled: (e.currentTarget as HTMLInputElement).checked })}
        />
        <span>
          <strong>Detailed log</strong>
          <span class="muted">Writes more detail to the log file while you track down a problem. Logs and crash reports stay on this computer.</span>
        </span>
      </label>
      <div class="inline">
        <button class="btn small" onclick={() => invoke("open_logs_folder")}>Open logs folder</button>
        <button class="btn small" onclick={() => invoke("report_problem")}>Report a problem…</button>
        <span class="muted">
          {diag.crash_reports.length ? `${diag.crash_reports.length} crash report${diag.crash_reports.length > 1 ? "s" : ""} saved` : "No crashes recorded"}
        </span>
      </div>
    {/if}
  </section>

  <section class="card">
    <h2>Video thumbnails</h2>
    {#if ffmpeg?.available}
      <p>FFmpeg {ffmpeg.version ?? ""} is available.<br /><span class="muted mono">{ffmpeg.ffmpeg}</span></p>
      <p class="muted">Video files get a frame as their thumbnail and full stream info. Drives enriched before FFmpeg was installed can be re-enriched from their card menu.</p>
    {:else}
      <p>FFmpeg is not installed. Without it, videos show a generic tile and only the container metadata Storecat can read itself (MP4, MOV, MKV).</p>
      {#if dl && dl.tool === "ffmpeg"}
        <div class="bar"><div style="width: {dl.total ? Math.round((dl.downloaded / dl.total) * 100) : 5}%"></div></div>
        <div class="muted">{bytes(dl.downloaded)}{dl.total ? ` of ${bytes(dl.total)}` : ""}</div>
      {:else}
        <button class="btn primary" disabled={downloading} onclick={download}>
          {downloading ? "Installing…" : "Download FFmpeg (about 150 MB)"}
        </button>
        <p class="muted small">A GPL build from the BtbN FFmpeg-Builds project, stored in Storecat's data folder. Or install FFmpeg on your PATH and it will be picked up.</p>
      {/if}
    {/if}
  </section>

  <section class="card">
    <h2>Catalogue</h2>
    <dl>
      <dt class="muted">Data folder</dt><dd class="mono">{info?.data_dir ?? "…"}</dd>
      <dt class="muted">Indexed items</dt><dd>{count(info?.index_docs ?? 0)}{store.stats.index_rebuilding ? " · rebuilding…" : ""}</dd>
      <dt class="muted">Drives</dt><dd>{store.stats.volumes} · {count(store.stats.files)} files · {bytes(store.stats.bytes)}</dd>
      <dt class="muted">Version</dt><dd>{info?.app_version ?? "…"} · core {info?.core_version ?? "…"} · {info?.platform ?? ""}</dd>
    </dl>
    <div class="row">
      <button class="btn" onclick={() => store.importCatalogue()}>Import a catalogue…</button>
    </div>
    <p class="muted small">A catalogue file comes from a drive card's "Export catalogue" or from the <code>.storecat</code> folder on a drive scanned on another computer.</p>
  </section>

  <section class="card wide">
    <h2>Search syntax</h2>
    <table>
      <tbody>
        <tr><td><code>invoice acme</code></td><td>all words, in names, folders, metadata or text</td></tr>
        <tr><td><code>"grand total"</code></td><td>exact phrase</td></tr>
        <tr><td><code>content:unpaid</code></td><td>inside documents only</td></tr>
        <tr><td><code>ext:pdf,docx</code></td><td>extensions</td></tr>
        <tr><td><code>kind:video</code></td><td>video, image, audio, document, archive, code, design, folder</td></tr>
        <tr><td><code>size:&gt;1gb</code> <code>size:1mb..5mb</code></td><td>sizes</td></tr>
        <tr><td><code>modified:2023-05</code> <code>after:2024</code> <code>modified:7d</code></td><td>dates</td></tr>
        <tr><td><code>drive:seagate</code></td><td>drive name, location or notes</td></tr>
        <tr><td><code>tag:client-x</code></td><td>tagged items; tag words also match plain searches</td></tr>
        <tr><td><code>path:clients/acme</code></td><td>folder path</td></tr>
        <tr><td><code>is:dir</code> <code>is:archived</code> <code>has:thumb</code> <code>has:text</code></td><td>kinds of items</td></tr>
      </tbody>
    </table>
  </section>
</div>

<style>
  .head {
    display: flex;
    align-items: baseline;
    gap: 16px;
  }
  h1 {
    font-size: 34px;
    font-weight: 700;
    line-height: 1.1;
  }
  h2 {
    font-size: 16px;
    font-weight: 700;
    margin-bottom: 4px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(380px, 1fr));
    gap: 14px;
    align-items: start;
  }
  .card {
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .card.wide {
    grid-column: 1 / -1;
  }
  .toggle,
  .field {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    font-size: 13px;
    cursor: pointer;
  }
  .toggle input {
    margin-top: 3px;
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
  }
  .toggle span,
  .field > span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .field {
    justify-content: space-between;
    align-items: center;
  }
  .field select {
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    color: var(--fg);
    font: inherit;
    font-size: 13px;
    padding: 6px 10px;
    outline: 0;
  }
  p {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
  }
  .small {
    font-size: 12px;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
    word-break: break-all;
  }
  .btn.small {
    height: 28px;
    font-size: 12px;
    padding: 0 12px;
    margin-top: 6px;
  }
  kbd {
    font-family: var(--font-mono);
    font-size: 12px;
    background: rgba(255, 255, 255, 0.07);
    padding: 1px 6px;
    border-radius: 5px;
    border: 1px solid var(--line-strong);
  }
  code {
    font-family: var(--font-mono);
    font-size: 12px;
    background: rgba(255, 255, 255, 0.07);
    padding: 1px 6px;
    border-radius: 5px;
  }
  dl {
    display: grid;
    grid-template-columns: 110px minmax(0, 1fr);
    gap: 8px 12px;
    margin: 0;
    font-size: 13px;
  }
  dd {
    margin: 0;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .bar {
    height: 8px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.08);
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
  table {
    border-collapse: collapse;
    font-size: 13px;
  }
  td {
    padding: 5px 18px 5px 0;
    vertical-align: top;
    color: var(--fg-soft);
  }
  td:first-child {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  td:first-child {
    white-space: nowrap;
  }
  .inline {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
</style>
