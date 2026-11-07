<script lang="ts">
  import EmptyState from "../components/EmptyState.svelte";

  let { title, body }: { title: string; body: string } = $props();
</script>

<h1>{title}</h1>
<EmptyState title="Coming later" {body} />

<style>
  h1 {
    font-size: 34px;
    font-weight: 700;
    line-height: 1.1;
  }
</style>
