<script lang="ts">
  import { updates } from "../updates.svelte";
</script>

{#if updates.available}
  <div class="banner" role="status">
    <span class="dot"></span>
    <span><strong>Storecat {updates.available.version}</strong> is ready to install.</span>
    <button class="link" onclick={() => updates.releaseNotes(updates.available!.version)}>What's new</button>
    <span class="grow"></span>
    {#if updates.progress !== null}
      <span class="muted">Downloading {Math.round(updates.progress * 100)}%…</span>
    {:else}
      <button class="btn primary sm" onclick={() => updates.install()}>Install and restart</button>
      <button class="btn sm" onclick={() => updates.dismiss()}>Later</button>
    {/if}
    {#if updates.error}<span class="err">{updates.error}</span>{/if}
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    border: 1px solid var(--accent-line);
    background: var(--accent-soft);
    border-radius: 14px;
    font-size: 13px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }
  .grow {
    flex: 1;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: #ffb39b;
    text-decoration: underline;
    font-size: 13px;
  }
  .btn.sm {
    height: 30px;
    padding: 0 12px;
    font-size: 12px;
  }
  .err {
    color: var(--danger);
    font-size: 12px;
  }
</style>
