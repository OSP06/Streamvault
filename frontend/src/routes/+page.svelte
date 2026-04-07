<script>
  import { onMount } from 'svelte';
  import UploadZone from '$lib/components/UploadZone.svelte';
  import VideoGrid from '$lib/components/VideoGrid.svelte';
  import Toast from '$lib/components/Toast.svelte';

  let videos = [];
  let toast = null;

  async function fetchVideos() {
    try {
      const res = await fetch('/api/videos');
      if (res.ok) videos = await res.json();
    } catch {}
  }

  onMount(fetchVideos);

  function onUploadSuccess(event) {
    const { result } = event.detail;
    showToast('success', 'Upload complete! Link copied to clipboard.');
    try { navigator.clipboard.writeText(`${location.origin}/watch/${result.token}`); } catch {}
    fetchVideos();
  }

  function onUploadError(event) {
    showToast('error', event.detail.message);
  }

  function showToast(type, message) {
    toast = { type, message };
    setTimeout(() => (toast = null), 4000);
  }
</script>

<svelte:head>
  <title>StreamVault — Private Video Streaming</title>
</svelte:head>

<div class="layout">
  <header>
    <div class="header-inner">
      <div class="logo">
        <span class="logo-icon">▶</span>
        <span class="logo-text">StreamVault</span>
        <span class="logo-badge">v0.1</span>
      </div>
      <span class="nav-hint mono">Anonymous · No auth required</span>
    </div>
  </header>

  <main>
    <section class="hero">
      <h1>Upload. Stream. Share.</h1>
      <p class="subtitle">
        Drop any video up to 1 GB. Get a private link instantly.
        No account required.
      </p>
    </section>

    <UploadZone on:success={onUploadSuccess} on:error={onUploadError} />

    {#if videos.length > 0}
      <section class="videos-section">
        <div class="section-header">
          <h2>Recent Uploads</h2>
          <span class="count mono">{videos.length} video{videos.length !== 1 ? 's' : ''}</span>
        </div>
        <VideoGrid {videos} />
      </section>
    {/if}
  </main>

  <footer>
    <span class="mono">StreamVault · Rust + Svelte</span>
    <div class="badges">
      <span class="badge">Axum</span>
      <span class="badge">SQLite</span>
      <span class="badge">HLS</span>
      <span class="badge">HTTP Range</span>
    </div>
  </footer>
</div>

{#if toast}
  <Toast type={toast.type} message={toast.message} />
{/if}

<style>
  .layout { min-height: 100vh; display: flex; flex-direction: column; }
  header { border-bottom: 1px solid var(--border); background: var(--surface); }
  .header-inner { max-width: 1100px; margin: 0 auto; padding: 0 24px; height: 56px; display: flex; align-items: center; justify-content: space-between; }
  .logo { display: flex; align-items: center; gap: 10px; }
  .logo-icon { width: 30px; height: 30px; background: var(--accent); border-radius: 6px; display: flex; align-items: center; justify-content: center; font-size: 12px; color: white; }
  .logo-text { font-weight: 600; font-size: 16px; letter-spacing: -0.02em; }
  .logo-badge { font-family: var(--mono); font-size: 11px; background: var(--surface-2); border: 1px solid var(--border); padding: 1px 6px; border-radius: 4px; color: var(--text-2); }
  .nav-hint { font-size: 12px; color: var(--text-3); }
  main { flex: 1; max-width: 1100px; margin: 0 auto; width: 100%; padding: 48px 24px; }
  .hero { text-align: center; margin-bottom: 48px; }
  .hero h1 { font-size: clamp(32px, 5vw, 52px); font-weight: 700; letter-spacing: -0.03em; line-height: 1.1; margin-bottom: 16px; background: linear-gradient(135deg, var(--text) 0%, var(--text-2) 100%); -webkit-background-clip: text; -webkit-text-fill-color: transparent; background-clip: text; }
  .subtitle { font-size: 16px; color: var(--text-2); max-width: 480px; margin: 0 auto; }
  .videos-section { margin-top: 64px; }
  .section-header { display: flex; align-items: baseline; gap: 12px; margin-bottom: 24px; padding-bottom: 12px; border-bottom: 1px solid var(--border); }
  .section-header h2 { font-size: 18px; font-weight: 600; letter-spacing: -0.02em; }
  .count { font-size: 12px; color: var(--text-3); }
  footer { border-top: 1px solid var(--border); padding: 20px 24px; display: flex; align-items: center; justify-content: space-between; font-size: 12px; color: var(--text-3); max-width: 1100px; margin: 0 auto; width: 100%; }
  .badges { display: flex; gap: 6px; }
  .badge { font-family: var(--mono); font-size: 10px; padding: 2px 7px; border-radius: 4px; border: 1px solid var(--border); color: var(--text-3); }
  .mono { font-family: var(--mono); }
</style>
