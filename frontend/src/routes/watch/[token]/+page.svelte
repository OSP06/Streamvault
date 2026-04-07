<script>
  import { page } from '$app/stores';
  import { onMount } from 'svelte';

  const token = $page.params.token;

  let video = null;
  let error = null;
  let copied = false;

  onMount(async () => {
    try {
      const res = await fetch(`/api/videos/${token}`);
      if (!res.ok) throw new Error('Video not found');
      video = await res.json();
    } catch (e) {
      error = e.message;
    }
  });

  $: streamUrl = video
    ? video.hls_ready
      ? `/api/hls/${token}/playlist.m3u8`
      : `/api/stream/${token}`
    : null;

  function formatBytes(bytes) {
    if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1073741824) return (bytes / 1048576).toFixed(1) + ' MB';
    return (bytes / 1073741824).toFixed(2) + ' GB';
  }

  function formatDate(s) {
    try { return new Date(s + 'Z').toLocaleString(); } catch { return s; }
  }

  async function copyLink() {
    await navigator.clipboard.writeText(window.location.href);
    copied = true;
    setTimeout(() => (copied = false), 2000);
  }
</script>

<svelte:head>
  <title>{video ? video.original_name : 'Loading…'} — StreamVault</title>
</svelte:head>

<div class="page">
  <header>
    <a class="back" href="/">← StreamVault</a>
  </header>

  <main>
    {#if error}
      <div class="error-state">
        <div class="error-icon">✗</div>
        <h2>Video not found</h2>
        <p>This link may be invalid or the video may have been removed.</p>
        <a href="/" class="btn-primary">Go home</a>
      </div>

    {:else if !video}
      <div class="loading">
        <div class="spinner"></div>
        <span>Loading…</span>
      </div>

    {:else}
      <div class="player-wrap">
        <!-- svelte-ignore a11y-media-has-caption -->
        <video class="player" src={streamUrl} controls preload="metadata" playsinline>
          Your browser does not support video playback.
        </video>
      </div>

      <div class="meta-bar">
        <div class="meta-left">
          <h1 class="video-title">{video.original_name}</h1>
          <div class="chips">
            <span class="chip mono">{formatBytes(video.size_bytes)}</span>
            <span class="chip mono">{video.content_type}</span>
            {#if video.hls_ready}
              <span class="chip chip--green mono">HLS ready</span>
            {:else}
              <span class="chip chip--amber mono">Direct stream</span>
            {/if}
            <span class="chip mono">{formatDate(video.created_at)}</span>
          </div>
        </div>
        <div class="meta-actions">
          <button class="action-btn" on:click={copyLink}>
            {copied ? '✓ Copied!' : '⎘ Copy link'}
          </button>
          <a class="action-btn" href="/api/stream/{token}" download>↓ Download</a>
        </div>
      </div>

      <div class="tech-note mono">
        <span class="label">Stream:</span>
        {#if video.hls_ready}
          HLS adaptive via <code>/api/hls/{token}/playlist.m3u8</code>
        {:else}
          HTTP byte-range via <code>/api/stream/{token}</code> — HLS transcode in progress
        {/if}
      </div>
    {/if}
  </main>
</div>

<style>
  .page { min-height: 100vh; display: flex; flex-direction: column; }
  header { border-bottom: 1px solid var(--border); padding: 0 24px; height: 52px; display: flex; align-items: center; background: var(--surface); }
  .back { font-size: 14px; color: var(--text-2); transition: color 0.15s; }
  .back:hover { color: var(--text); text-decoration: none; }
  main { flex: 1; max-width: 960px; margin: 0 auto; width: 100%; padding: 32px 24px 48px; }
  .player-wrap { background: #000; border-radius: var(--radius-lg); overflow: hidden; margin-bottom: 20px; border: 1px solid var(--border); aspect-ratio: 16/9; }
  .player { width: 100%; height: 100%; display: block; outline: none; }
  .meta-bar { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 16px; flex-wrap: wrap; }
  .video-title { font-size: 18px; font-weight: 600; letter-spacing: -0.02em; margin-bottom: 8px; }
  .chips { display: flex; gap: 6px; flex-wrap: wrap; }
  .chip { font-size: 11px; padding: 3px 8px; border-radius: 4px; border: 1px solid var(--border); color: var(--text-3); background: var(--surface); }
  .chip--green { border-color: rgba(34,197,94,.3); color: var(--green); background: rgba(34,197,94,.07); }
  .chip--amber { border-color: rgba(245,158,11,.3); color: var(--amber); background: rgba(245,158,11,.07); }
  .meta-actions { display: flex; gap: 8px; flex-shrink: 0; }
  .action-btn { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); padding: 8px 14px; font-size: 13px; color: var(--text); cursor: pointer; transition: border-color 0.15s, color 0.15s; text-decoration: none; display: inline-flex; align-items: center; gap: 5px; white-space: nowrap; }
  .action-btn:hover { border-color: var(--accent-dim); color: var(--accent); text-decoration: none; }
  .tech-note { font-size: 11px; color: var(--text-3); border-top: 1px solid var(--border); padding-top: 12px; }
  .label { color: var(--text-2); margin-right: 6px; }
  code { background: var(--surface-2); padding: 1px 5px; border-radius: 3px; font-size: 11px; }
  .error-state { text-align: center; padding: 80px 24px; }
  .error-icon { font-size: 40px; color: var(--red); margin-bottom: 16px; }
  .error-state h2 { font-size: 22px; font-weight: 600; margin-bottom: 8px; }
  .error-state p { color: var(--text-2); margin-bottom: 24px; }
  .btn-primary { background: var(--accent); color: white; padding: 10px 20px; border-radius: var(--radius); font-size: 14px; text-decoration: none; }
  .loading { display: flex; align-items: center; justify-content: center; gap: 12px; padding: 80px; color: var(--text-3); }
  .spinner { width: 20px; height: 20px; border: 2px solid var(--border); border-top-color: var(--accent); border-radius: 50%; animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .mono { font-family: var(--mono); }
</style>
