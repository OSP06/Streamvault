<script>
  export let videos = [];

  function formatBytes(b) {
    if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
    if (b < 1073741824) return (b / 1048576).toFixed(1) + ' MB';
    return (b / 1073741824).toFixed(2) + ' GB';
  }

  function formatDate(s) {
    try { return new Date(s + 'Z').toLocaleDateString('en-US', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }); }
    catch { return s; }
  }

  function mimeTag(ct) {
    if (ct.includes('mp4')) return 'MP4';
    if (ct.includes('webm')) return 'WEBM';
    if (ct.includes('quicktime')) return 'MOV';
    if (ct.includes('matroska')) return 'MKV';
    if (ct.includes('avi')) return 'AVI';
    return 'VID';
  }
</script>

<div class="grid">
  {#each videos as video (video.token)}
    <a class="card" href="/watch/{video.token}" title="Watch {video.original_name}">
      <div class="thumb">
        <span class="format mono">{mimeTag(video.content_type)}</span>
        <div class="play">▶</div>
        {#if video.hls_ready}
          <span class="hls-badge mono">HLS</span>
        {/if}
      </div>
      <div class="card-body">
        <p class="name" title={video.original_name}>{video.original_name}</p>
        <div class="meta mono">
          <span>{formatBytes(video.size_bytes)}</span>
          <span>·</span>
          <span>{formatDate(video.created_at)}</span>
        </div>
        <span class="token mono">{video.token}</span>
      </div>
    </a>
  {/each}
</div>

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 16px; }
  .card { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg); overflow: hidden; text-decoration: none; color: inherit; display: flex; flex-direction: column; transition: border-color 0.2s, transform 0.15s; }
  .card:hover { border-color: var(--accent-dim); transform: translateY(-2px); text-decoration: none; }
  .thumb { height: 120px; background: var(--surface-2); position: relative; display: flex; align-items: center; justify-content: center; border-bottom: 1px solid var(--border); }
  .format { position: absolute; top: 8px; left: 8px; font-size: 10px; background: var(--bg); border: 1px solid var(--border); padding: 2px 6px; border-radius: 4px; color: var(--text-3); }
  .play { width: 40px; height: 40px; background: var(--accent-glow); border: 1px solid var(--accent-dim); border-radius: 50%; display: flex; align-items: center; justify-content: center; font-size: 14px; color: var(--accent); padding-left: 3px; transition: background 0.15s; }
  .card:hover .play { background: var(--accent); color: white; border-color: var(--accent); }
  .hls-badge { position: absolute; bottom: 8px; right: 8px; font-size: 9px; background: rgba(34,197,94,.15); border: 1px solid rgba(34,197,94,.3); color: var(--green); padding: 2px 6px; border-radius: 4px; }
  .card-body { padding: 12px 14px; }
  .name { font-size: 13px; font-weight: 500; margin-bottom: 4px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; gap: 6px; font-size: 11px; color: var(--text-3); margin-bottom: 8px; }
  .token { font-size: 10px; color: var(--text-3); border: 1px solid var(--border); display: inline-block; padding: 1px 6px; border-radius: 3px; }
  .mono { font-family: var(--mono); }
</style>
