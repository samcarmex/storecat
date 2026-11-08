<script lang="ts">
  import { onMount, tick } from "svelte";
  import { ops, type ConflictPolicy, type Job, type JobItem, type JobKind, type JobPlan, type JobProgress, type JobSpec } from "../api";
  import type { AppStore } from "../store.svelte";
  import { ago, bytes, count, duration, MEDIA_COLOR, MEDIA_LABEL } from "../format";
  import EmptyState from "../components/EmptyState.svelte";

  let { store, prefill = $bindable(null) }: { store: AppStore; prefill?: JobSpec | null } = $props();

  // ----- new job form -----
  let kind = $state<JobKind>("copy");
  let conflict = $state<ConflictPolicy>("rename");
  let sources = $state<string[]>([]);
  let destVolumeId = $state<number | null>(null);
  let destSub = $state("");
  let name = $state("");
  let plan = $state<JobPlan | null>(null);
  let planning = $state(false);
  let starting = $state(false);
  let formError = $state<string | null>(null);

  // Real drives first (a backup destination is normally another disk), then folders.
  const online = $derived(
    store.volumes
      .filter((v) => v.online && v.mount_path)
      .sort((a, b) => Number(b.source === "device") - Number(a.source === "device")),
  );
  const destVolume = $derived(online.find((v) => v.id === destVolumeId) ?? null);
  const destRoot = $derived.by(() => {
    if (!destVolume?.mount_path) return "";
    const base = destVolume.mount_path.replace(/[\\/]+$/, "");
    const sub = destSub.replace(/^[\\/]+/, "");
    return sub ? `${base}\\${sub}` : base;
  });
  const spec = $derived<JobSpec>({
    kind,
    name: name.trim() || (kind === "move" ? "Move" : "Back up") + (sources.length ? ` ${sources.length} item${sources.length > 1 ? "s" : ""}` : ""),
    sources,
    dest_root: destRoot,
    conflict,
    verify: true,
  });

  $effect(() => {
    if (destVolumeId === null && online.length > 0) destVolumeId = online[0].id;
  });

  // A job handed over from a storage suggestion: fill the form and show its plan.
  $effect(() => {
    const p = prefill;
    if (!p) return;
    prefill = null;
    void loadPrefill(p);
  });

  // A cleanup handed over from a suggestion: files to send to the Recycle Bin.
  let cleanup = $state<JobSpec | null>(null);
  let cleanupPlan = $state<JobPlan | null>(null);

  async function loadCleanup(p: JobSpec) {
    cleanup = p;
    cleanupPlan = null;
    formError = null;
    planning = true;
    try {
      cleanupPlan = await ops.planJob(p);
    } catch (e) {
      formError = String(e);
    } finally {
      planning = false;
    }
  }

  async function startCleanup() {
    if (!cleanup) return;
    starting = true;
    formError = null;
    try {
      await ops.createJob(cleanup, true);
      cleanup = null;
      cleanupPlan = null;
      await loadJobs();
    } catch (e) {
      formError = String(e);
    } finally {
      starting = false;
    }
  }

  async function loadPrefill(p: JobSpec) {
    if (p.kind === "delete") return loadCleanup(p);
    cleanup = null;
    kind = p.kind === "move" ? "move" : "copy";
    conflict = p.conflict;
    sources = [...p.sources];
    name = p.name;
    plan = null;
    formError = null;
    const norm = (x: string) => x.replace(/[\\/]+$/, "").toLowerCase();
    const target = norm(p.dest_root);
    const match = online
      .filter((v) => {
        if (!v.mount_path) return false;
        const m = norm(v.mount_path);
        return target === m || target.startsWith(m + "\\") || target.startsWith(m + "/");
      })
      .sort((a, b) => b.mount_path!.length - a.mount_path!.length)[0];
    if (!match?.mount_path) {
      formError = "The destination drive for this suggestion isn't connected.";
      return;
    }
    destVolumeId = match.id;
    destSub = p.dest_root.slice(match.mount_path.replace(/[\\/]+$/, "").length).replace(/^[\\/]+/, "");
    await tick();
    await preview();
  }

  async function usePreset() {
    if (!destVolume?.mount_path) return;
    try {
      const s = await ops.sweepSpec(destVolume.mount_path, kind);
      sources = s.sources;
      name = s.name;
      const base = destVolume.mount_path.replace(/[\\/]+$/, "");
      destSub = s.dest_root.startsWith(base) ? s.dest_root.slice(base.length).replace(/^[\\/]+/, "") : s.dest_root;
      plan = null;
      await preview();
    } catch (e) {
      formError = String(e);
    }
  }

  async function addFolders() {
    try {
      const picked = await ops.pickFolders();
      sources = [...new Set([...sources, ...picked])];
      plan = null;
    } catch (e) {
      formError = String(e);
    }
  }

  async function addFiles() {
    try {
      const picked = await ops.pickFiles();
      sources = [...new Set([...sources, ...picked])];
      plan = null;
    } catch (e) {
      formError = String(e);
    }
  }

  function removeSource(s: string) {
    sources = sources.filter((x) => x !== s);
    plan = null;
  }

  async function preview() {
    if (!sources.length || !destRoot) return;
    planning = true;
    formError = null;
    try {
      plan = await ops.planJob(spec);
    } catch (e) {
      formError = String(e);
    } finally {
      planning = false;
    }
  }

  async function start() {
    starting = true;
    formError = null;
    try {
      await ops.createJob(spec, true);
      sources = [];
      plan = null;
      name = "";
      await loadJobs();
    } catch (e) {
      formError = String(e);
    } finally {
      starting = false;
    }
  }

  // ----- job list -----
  let jobs = $state<Job[]>([]);
  let progress = $state<Record<number, JobProgress>>({});
  let openJob = $state<number | null>(null);
  let items = $state<JobItem[]>([]);

  async function loadJobs() {
    jobs = await ops.jobs();
  }

  async function toggleItems(id: number) {
    if (openJob === id) {
      openJob = null;
      return;
    }
    openJob = id;
    items = await ops.jobItems(id, undefined, 300);
  }

  onMount(() => {
    void loadJobs();
    const un: Array<() => void> = [];
    ops.onJobsChanged(() => void loadJobs()).then((u) => un.push(u));
    ops.onJobProgress((p) => {
      progress[p.job_id] = p;
      if (p.status !== "running") {
        setTimeout(() => {
          if (progress[p.job_id]?.status === p.status) delete progress[p.job_id];
        }, 4000);
      }
    }).then((u) => un.push(u));
    return () => un.forEach((u) => u());
  });

  /** What a finished job achieved, in one line. */
  function outcome(j: Job): string {
    const files = `${count(j.files_done)} ${j.files_done === 1 ? "file" : "files"}`;
    if (j.status === "cancelled") return `Cancelled after ${count(j.files_done)} of ${count(j.files_total)} files`;
    if (j.status === "failed") return `${count(j.files_done)} of ${count(j.files_total)} files done · ${count(j.files_failed)} failed`;
    if (j.kind === "delete") return `${bytes(j.bytes_done)} freed · ${files} recycled`;
    if (j.kind === "move") return `${bytes(j.bytes_done)} moved · ${files}${j.verify ? ", each verified" : ""}`;
    return `${bytes(j.bytes_done)} copied · ${files}${j.verify ? ", each verified" : ""}`;
  }

  const STATUS_LABEL: Record<string, string> = {
    planned: "Ready", running: "Running", paused: "Paused", done: "Done", failed: "Finished with errors", cancelled: "Cancelled",
  };
</script>

<div class="head">
  <h1>Back it up, <em>then let go.</em></h1>
  <span class="muted">Copy or move with verification; every file is journaled so a job can pause and resume.</span>
</div>

<div class="layout">
  <section class="card form" aria-label="New job">
    {#if cleanup}
    <h2>Clean up</h2>
    <p class="cleanup-name">{cleanup.name}</p>
    {#if cleanupPlan}
      <div class="plan">
        <div><strong>{count(cleanupPlan.files)}</strong> files · <strong>{bytes(cleanupPlan.bytes)}</strong> to the Recycle Bin</div>
        <div class="muted">
          {#if cleanupPlan.missing_sources.length}{count(cleanupPlan.missing_sources.length)} already gone · {/if}
          Files that no longer match what they were picked for are left in place.
        </div>
        <div class="sample mono">
          {#each cleanup.sources.slice(0, 6) as s (s)}
            <div>{s}</div>
          {/each}
          {#if cleanup.sources.length > 6}<div class="muted">and {count(cleanup.sources.length - 6)} more</div>{/if}
        </div>
      </div>
    {:else if planning}
      <div class="muted">Checking…</div>
    {/if}
    {#if formError}<div class="error">{formError}</div>{/if}
    <div class="actions">
      <button class="btn" onclick={() => { cleanup = null; cleanupPlan = null; formError = null; }}>Cancel</button>
      <button class="btn primary" onclick={startCleanup} disabled={!cleanupPlan || cleanupPlan.files === 0 || starting}>
        {starting ? "Starting…" : `Recycle ${cleanupPlan ? count(cleanupPlan.files) : ""} files`}
      </button>
    </div>
    {:else}
    <h2>New job</h2>

    <div class="row">
      <div class="seg" role="radiogroup" aria-label="Mode">
        <button class:on={kind === "copy"} onclick={() => { kind = "copy"; plan = null; }}>Copy <span class="muted">keep originals</span></button>
        <button class:on={kind === "move"} onclick={() => { kind = "move"; plan = null; }}>Move <span class="muted">copy, verify, delete</span></button>
      </div>
    </div>

    <div class="field">
      <span class="lbl">What</span>
      <div class="sources">
        {#each sources as s (s)}
          <span class="chip mono" title={s}>{s}<button aria-label="Remove" onclick={() => removeSource(s)}>×</button></span>
        {/each}
        <button class="btn small" onclick={addFolders}>+ Folders…</button>
        <button class="btn small" onclick={addFiles}>+ Files…</button>
        <button class="btn small ghost" onclick={usePreset} disabled={!destVolume} title="Desktop and Downloads into a dated Archive folder on the destination">Use the Desktop + Downloads preset</button>
      </div>
    </div>

    <div class="field">
      <span class="lbl">Where</span>
      <div class="dest">
        {#if online.length === 0}
          <span class="muted">Connect a catalogued drive to use as the destination.</span>
        {:else}
          <select bind:value={destVolumeId} onchange={() => (plan = null)} aria-label="Destination drive">
            {#each online as v (v.id)}
              <option value={v.id}>{v.label} · {v.mount_path}{v.capacity > 0 ? ` · ${bytes(v.free)} free` : ""}</option>
            {/each}
          </select>
          <input bind:value={destSub} oninput={() => (plan = null)} placeholder="subfolder, e.g. Archive\2026-11 Desktop+Downloads" aria-label="Destination subfolder" />
        {/if}
      </div>
    </div>

    <div class="field">
      <span class="lbl">Options</span>
      <div class="opts">
        <label>If a file exists
          <select bind:value={conflict} onchange={() => (plan = null)}>
            <option value="rename">keep both (rename)</option>
            <option value="skip">skip</option>
            <option value="overwrite">overwrite</option>
          </select>
        </label>
        <input bind:value={name} placeholder="Job name (optional)" aria-label="Job name" />
      </div>
    </div>

    {#if plan}
      <div class="plan" class:bad={!plan.enough_space}>
        <div><strong>{count(plan.files)}</strong> files in <strong>{count(plan.folders)}</strong> folders · <strong>{bytes(plan.bytes)}</strong></div>
        <div class="muted">
          {#if plan.dest_free !== null}{bytes(plan.dest_free)} free at the destination{plan.enough_space ? "" : " — not enough"}{/if}
          {#if plan.conflicts}· {plan.conflicts} already exist ({conflict}){/if}
          {#if plan.missing_sources.length}· {plan.missing_sources.length} unreadable{/if}
        </div>
        {#if plan.sample.length}
          <div class="sample mono">
            {#each plan.sample.slice(0, 4) as [s, d] (s)}
              <div>{s}<br />→ {d}</div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    {#if formError}<div class="error">{formError}</div>{/if}

    <div class="actions">
      <button class="btn" onclick={preview} disabled={!sources.length || !destRoot || planning}>{planning ? "Checking…" : "Preview"}</button>
      <button class="btn primary" onclick={start} disabled={!plan || !plan.enough_space || plan.files === 0 || starting}>
        {starting ? "Starting…" : kind === "move" ? "Move and verify" : "Copy and verify"}
      </button>
    </div>
    {/if}
  </section>

  <section class="jobs" aria-label="Jobs">
    <h2>Jobs</h2>
    {#if jobs.length === 0}
      <EmptyState title="No jobs yet" body="Jobs you start appear here with live progress, and stay as a record of what moved where." />
    {:else}
      {#each jobs as j (j.id)}
        {@const p = progress[j.id]}
        {@const live = p?.status === "running"}
        {@const filesDone = live ? p.files_done : j.files_done}
        {@const bytesDone = live ? p.bytes_done : j.bytes_done}
        {@const pct = j.bytes_total > 0 ? Math.min(100, (bytesDone / j.bytes_total) * 100) : (j.files_total ? (filesDone / j.files_total) * 100 : 0)}
        {@const finished = !live && (j.status === "done" || j.status === "failed" || j.status === "cancelled")}
        <article class="card job" class:failed={j.status === "failed"} class:complete={finished && j.status === "done"}>
          <header>
            <span class="kind">{j.kind}</span>
            <span class="name">{j.name}</span>
            <span class="status muted">{live ? "Running" : STATUS_LABEL[j.status] ?? j.status}{j.status !== "running" && j.finished_at ? ` · ${ago(j.finished_at)}` : ""}</span>
          </header>
          {#if finished}
            <div class="outcome" class:ok={j.status === "done"} class:bad={j.status === "failed"}>
              <span class="mark" aria-hidden="true">{j.status === "done" ? "✓" : j.status === "failed" ? "!" : "–"}</span>
              <span>{outcome(j)}</span>
            </div>
            {#if j.files_skipped}<div class="muted line">{count(j.files_skipped)} skipped</div>{/if}
          {:else}
          <div class="bar"><div style="width: {pct}%"></div></div>
          <div class="muted line">
            {count(filesDone)} / {count(j.files_total)} files · {bytes(bytesDone)} of {bytes(j.bytes_total)}
            {#if live && p.bytes_per_sec > 0}· {bytes(p.bytes_per_sec)}/s{/if}
            {#if j.files_failed}· <span class="danger">{j.files_failed} failed</span>{/if}
            {#if j.files_skipped}· {j.files_skipped} skipped{/if}
            {#if live && p.current}· <span class="cur">{p.current}</span>{/if}
          </div>
          {/if}
          {#if j.dest_root}<div class="muted mono small">→ {j.dest_root}</div>{/if}
          {#if j.error && !live}<div class="error small">{j.error}</div>{/if}
          <footer>
            {#if live}
              <button class="btn small" onclick={() => ops.pauseJob(j.id)}>Pause</button>
              <button class="btn small ghost" onclick={() => ops.cancelJob(j.id)}>Cancel</button>
            {:else if j.status === "planned" || j.status === "paused" || j.status === "failed"}
              <button class="btn small primary" onclick={() => ops.runJob(j.id)}>{j.status === "planned" ? "Start" : "Resume"}</button>
              <button class="btn small ghost" onclick={() => ops.cancelJob(j.id)}>Cancel</button>
            {/if}
            <span class="grow"></span>
            <button class="btn small ghost" onclick={() => toggleItems(j.id)}>{openJob === j.id ? "Hide files" : "Files"}</button>
            {#if !live}
              <button class="btn small ghost" onclick={() => ops.deleteJob(j.id)}>Remove</button>
            {/if}
          </footer>
          {#if openJob === j.id}
            <ul class="items">
              {#each items as it (it.id)}
                <li class={it.status}>
                  <span class="st">{it.status}</span>
                  <span class="mono src">{it.source}</span>
                  <span class="muted">{bytes(it.size)}</span>
                  {#if it.error}<span class="danger">{it.error}</span>{/if}
                </li>
              {/each}
              {#if items.length >= 300}<li class="muted">Showing the first 300 files.</li>{/if}
            </ul>
          {/if}
        </article>
      {/each}
    {/if}
  </section>
</div>

<style>
  .cleanup-name {
    margin: 0;
    font-weight: 600;
  }
  .head { display: flex; align-items: flex-end; gap: 24px; flex-wrap: wrap; }
  h1 { font-size: 34px; font-weight: 700; line-height: 1.1; }
  h1 em { font-style: normal; color: var(--accent); }
  .head .muted { padding-bottom: 6px; }
  h2 { font-size: 16px; font-weight: 700; }
  .layout { display: grid; grid-template-columns: minmax(380px, 520px) minmax(0, 1fr); gap: var(--gap); align-items: start; }
  @media (max-width: 1100px) { .layout { grid-template-columns: 1fr; } }
  .form { padding: 18px 20px; display: flex; flex-direction: column; gap: 14px; }
  .seg { display: flex; width: 100%; padding: 4px; border-radius: 14px; background: rgba(255,255,255,.05); gap: 4px; }
  .seg button { flex: 1; border: 0; background: transparent; color: var(--fg-soft); border-radius: 10px; padding: 10px 16px; font-size: 14px; font-weight: 600; display: flex; flex-direction: column; align-items: flex-start; gap: 3px; text-align: left; }
  .seg button .muted { font-weight: 400; font-size: 11px; }
  .seg button.on { background: var(--accent); color: var(--on-accent); }
  .seg button.on .muted { color: var(--on-accent); opacity: .75; }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .lbl { font-size: 11px; font-weight: 700; letter-spacing: .06em; text-transform: uppercase; color: var(--fg-muted); }
  .sources { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .chip { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; padding: 4px 6px 4px 10px; border-radius: 10px; background: rgba(255,255,255,.07); font-size: 12px; overflow: hidden; }
  .chip button { all: unset; cursor: pointer; width: 18px; height: 18px; border-radius: 50%; text-align: center; line-height: 18px; color: var(--fg-muted); }
  .chip button:hover { background: rgba(255,255,255,.12); color: var(--fg); }
  .mono { font-family: var(--font-mono); font-size: 12px; }
  .dest, .opts { display: flex; flex-direction: column; gap: 6px; }
  .opts { flex-direction: row; flex-wrap: wrap; align-items: center; gap: 10px; font-size: 13px; }
  .opts label { display: flex; gap: 8px; align-items: center; }
  select, input { background: rgba(0,0,0,.25); border: 1px solid var(--line-strong); border-radius: 8px; color: var(--fg); font: inherit; font-size: 13px; padding: 7px 10px; outline: 0; min-width: 0; }
  select option { background: var(--surface-2); }
  input:focus, select:focus { border-color: var(--accent-line); }
  .opts input { flex: 1; }
  .plan { padding: 12px 14px; border-radius: 12px; background: rgba(94,224,200,.08); border: 1px solid rgba(94,224,200,.3); font-size: 13px; display: flex; flex-direction: column; gap: 6px; }
  .plan.bad { background: rgba(255,92,92,.08); border-color: rgba(255,92,92,.4); }
  .sample { color: var(--fg-muted); display: flex; flex-direction: column; gap: 4px; max-height: 120px; overflow: auto; }
  .actions { display: flex; gap: 8px; }
  .btn.small { height: 30px; font-size: 12px; padding: 0 12px; }
  .btn.ghost { border-color: transparent; color: var(--fg-muted); }
  .error { color: var(--danger); font-size: 13px; }
  .small { font-size: 12px; }
  .danger { color: var(--danger); }
  .jobs { display: flex; flex-direction: column; gap: 12px; min-width: 0; }
  .job { padding: 14px 18px; display: flex; flex-direction: column; gap: 8px; }
  .job.failed { border-color: rgba(255,92,92,.35); }
  .job header { display: flex; gap: 10px; align-items: baseline; }
  .kind { font-family: var(--font-display); font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; color: var(--teal); }
  .name { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .job.complete { border-color: rgba(80, 200, 120, .28); }
  .outcome { display: flex; align-items: center; gap: 10px; font-size: 15px; font-weight: 600; }
  .outcome .mark { width: 22px; height: 22px; border-radius: 50%; display: inline-grid; place-items: center; font-size: 13px; background: rgba(255,255,255,.08); color: var(--fg-soft); }
  .outcome.ok .mark { background: rgba(80, 200, 120, .18); color: #5fd38d; }
  .outcome.bad .mark { background: rgba(255, 92, 92, .18); color: #ff7a7a; }
  .bar { height: 8px; border-radius: 4px; background: rgba(255,255,255,.08); overflow: hidden; }
  .bar div { height: 100%; background: var(--accent); border-radius: 4px; transition: width .3s; }
  .line { font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .cur { color: var(--fg-soft); }
  footer { display: flex; gap: 8px; align-items: center; }
  .grow { flex: 1; }
  .items { list-style: none; margin: 4px 0 0; padding: 8px 0 0; border-top: 1px solid var(--line); display: flex; flex-direction: column; gap: 4px; max-height: 320px; overflow: auto; font-size: 12px; }
  .items li { display: flex; gap: 10px; align-items: center; min-width: 0; }
  .st { width: 64px; flex-shrink: 0; font-size: 11px; color: var(--fg-muted); }
  .items li.done .st { color: var(--teal); }
  .items li.failed .st { color: var(--danger); }
  .src { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
