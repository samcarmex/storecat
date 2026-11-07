<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    title,
    body,
    children,
  }: { title: string; body: string; children?: Snippet } = $props();
</script>

<section class="card empty">
  <div class="art" aria-hidden="true">
    <span class="bar a"></span>
    <span class="bar b"></span>
    <span class="bar c"></span>
  </div>
  <h2>{title}</h2>
  <p class="muted">{body}</p>
  {#if children}
    <div class="actions">{@render children()}</div>
  {/if}
</section>

<style>
  .empty {
    padding: 48px 32px;
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 10px;
  }
  .art {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 10px;
  }
  .bar {
    display: block;
    height: 12px;
    border-radius: 6px;
  }
  .a { width: 96px; background: var(--accent); }
  .b { width: 64px; background: var(--teal); opacity: 0.9; }
  .c { width: 80px; background: var(--amber); opacity: 0.8; }
  h2 {
    font-size: 20px;
    font-weight: 700;
  }
  p {
    max-width: 440px;
    font-size: 13px;
    line-height: 1.5;
    margin: 0;
  }
  .actions {
    margin-top: 12px;
    display: flex;
    gap: 8px;
  }
</style>
