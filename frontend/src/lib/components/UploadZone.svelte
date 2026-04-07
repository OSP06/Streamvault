<script>
  import { createEventDispatcher } from 'svelte';

  const dispatch = createEventDispatcher();

  const MAX_SIZE = 1024 * 1024 * 1024;
  const ALLOWED_EXTS = ['mp4', 'webm', 'mov', 'avi', 'mkv', 'ts', 'mpeg', 'mpg', 'm4v', 'ogg'];

  let dragOver = false;
  let uploading = false;
  let progress = 0;
  let result = null;
  let inputEl;

  function onDragEnter(e) { e.preventDefault(); dragOver = true; }
  function onDragLeave(e) {
    e.preventDefault();
    if (!e.currentTarget.contains(e.relatedTarget)) dragOver = false;
  }
  function onDrop(e) {
    e.preventDefault(); dragOver = false;
    const file = e.dataTransfer?.files[0];
    if (file) upload(file);
  }
  function onFileChange(e) {
    const file = e.target.files?.[0];
    if (file) upload(file);
  }

  function validate(file) {
    const ext = file.name.split('.').pop()?.toLowerCase() ?? '';
    if (!ALLOWED_EXTS.includes(ext)) {
      dispatch('error', { message: `Unsupported format (.${ext}). Allowed: ${ALLOWED_EXTS.join(', ')}` });
      return false;
    }
    if (file.size > MAX_SIZE) {
      dispatch('error', { message: 'File exceeds 1 GB limit.' });
      return false;
    }
    return true;
  }

  function upload(file) {
    if (!validate(file)) return;
    uploading = true; progress = 0; result = null;

    const fd = new FormData();
    fd.append('video', file);

    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/upload');

    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) progress = Math.round((e.loaded / e.total) * 100);
    };

    xhr.onload = () => {
      uploading = false;
      if (xhr.status >= 200 && xhr.status < 300) {
        result = JSON.parse(xhr.responseText);
        dispatch('success', { result });
      } else {
        try { dispatch('error', { message: JSON.parse(xhr.responseText).error }); }
        catch { dispatch('error', { message: `Upload failed (${xhr.status})` }); }
      }
      if (inputEl) inputEl.value = '';
    };

    xhr.onerror = () => { uploading = false; dispatch('error', { message: 'Network error during upload.' }); };
    xhr.send(fd);
  }

  function copyLink() {
    if (result) navigator.clipboard.writeText(`${location.origin}/watch/${result.token}`);
  }

  function reset() { result = null; progress = 0; }

  function formatBytes(b) {
    if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
    if (b < 1073741824) return (b / 1048576).toFixed(1) + ' MB';
    return (b / 1073741824).toFixed(2) + ' GB';
  }
</script>

<div class="wrap">
  {#if result}
    <div class="success-card">
      <div class="success-icon">✓</div>
      <div class="body">
        <h3>Upload complete</h3>
        <p class="filename">{result.original_name}</p>
        <p class="filesize mono">{formatBytes(result.size_bytes)}</p>
        <div class="link-row">
          <input class="link-input mono" readonly value="{location.origin}/watch/{result.token}" />
          <button class="copy-btn" on:click={copyLink}>Copy</button>
        </div>
        <div class="action-row">
          <a class="watch-btn" href="/watch/{result.token}">▶ Watch Now</a>
          <button class="reset-btn" on:click={reset}>Upload another</button>
        </div>
      </div>
    </div>

  {:else if uploading}
    <div class="progress-card">
      <div class="progress-header">
        <span>Uploading…</span>
        <span class="mono">{progress}%</span>
      </div>
      <div class="progress-bar-wrap">
        <div class="progress-bar" style="width: {progress}%"></div>
      </div>
      <p class="progress-note mono">Streaming to server — playable immediately on completion.</p>
    </div>

  {:else}
    <!-- svelte-ignore a11y-no-noninteractive-tabindex -->
    <div
      class="dropzone"
      class:drag-active={dragOver}
      tabindex="0"
      role="button"
      on:dragenter={onDragEnter}
      on:dragover|preventDefault
      on:dragleave={onDragLeave}
      on:drop={onDrop}
      on:keydown={(e) => e.key === 'Enter' && inputEl?.click()}
    >
      <input bind:this={inputEl} type="file" accept="video/*" hidden on:change={onFileChange} />
      <div class="drop-icon">
        <svg width="40" height="40" viewBox="0 0 40 40" fill="none">
          <rect width="40" height="40" rx="8" fill="rgba(99,102,241,0.15)"/>
          <path d="M20 8L20 26M13 19L20 26L27 19" stroke="#6366f1" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
          <path d="M10 30L30 30" stroke="#6366f1" stroke-width="2" stroke-linecap="round" opacity="0.5"/>
        </svg>
      </div>
      <p class="drop-title">Drop your video here</p>
      <p class="drop-sub">or <button class="file-btn" on:click={() => inputEl?.click()}>browse files</button></p>
      <div class="constraints mono">
        <span>Max 1 GB</span><span>·</span><span>{ALLOWED_EXTS.join(' · ')}</span>
      </div>
    </div>
  {/if}
</div>

<style>
  .wrap { max-width: 680px; margin: 0 auto; }
  .dropzone { border: 2px dashed var(--border); border-radius: var(--radius-lg); background: var(--surface); padding: 56px 32px; text-align: center; cursor: pointer; transition: border-color 0.2s, background 0.2s; outline: none; }
  .dropzone:hover, .dropzone:focus { border-color: var(--accent-dim); background: var(--surface-2); }
  .drag-active { border-color: var(--accent) !important; background: var(--accent-glow) !important; }
  .drop-icon { margin-bottom: 16px; display: flex; justify-content: center; }
  .drop-title { font-size: 17px; font-weight: 600; margin-bottom: 6px; letter-spacing: -0.02em; }
  .drop-sub { font-size: 14px; color: var(--text-2); margin-bottom: 20px; }
  .file-btn { background: none; border: none; color: var(--accent); font-size: 14px; padding: 0; text-decoration: underline; cursor: pointer; font-family: var(--sans); }
  .constraints { font-size: 11px; color: var(--text-3); display: flex; gap: 8px; justify-content: center; flex-wrap: wrap; }
  .progress-card { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 32px; }
  .progress-header { display: flex; justify-content: space-between; font-weight: 500; margin-bottom: 12px; font-size: 14px; }
  .progress-bar-wrap { height: 6px; background: var(--surface-2); border-radius: 3px; overflow: hidden; margin-bottom: 12px; }
  .progress-bar { height: 100%; background: var(--accent); border-radius: 3px; transition: width 0.15s; box-shadow: 0 0 12px var(--accent-glow); }
  .progress-note { font-size: 12px; color: var(--text-3); }
  .success-card { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 32px; display: flex; gap: 20px; align-items: flex-start; }
  .success-icon { width: 40px; height: 40px; background: rgba(34,197,94,.15); color: var(--green); border-radius: 50%; display: flex; align-items: center; justify-content: center; font-size: 18px; flex-shrink: 0; }
  .body { flex: 1; min-width: 0; }
  .body h3 { font-size: 16px; font-weight: 600; margin-bottom: 4px; }
  .filename { font-size: 13px; color: var(--text-2); margin-bottom: 2px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .filesize { font-size: 11px; color: var(--text-3); margin-bottom: 16px; }
  .link-row { display: flex; gap: 8px; margin-bottom: 16px; }
  .link-input { flex: 1; background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius); padding: 8px 12px; font-size: 12px; color: var(--text-2); min-width: 0; outline: none; }
  .copy-btn { background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius); padding: 8px 14px; font-size: 13px; color: var(--text); transition: border-color 0.15s, color 0.15s; }
  .copy-btn:hover { border-color: var(--accent); color: var(--accent); }
  .action-row { display: flex; gap: 10px; align-items: center; }
  .watch-btn { background: var(--accent); color: white; border-radius: var(--radius); padding: 8px 16px; font-size: 13px; font-weight: 500; text-decoration: none; display: inline-flex; align-items: center; gap: 6px; transition: background 0.15s; }
  .watch-btn:hover { background: var(--accent-dim); text-decoration: none; }
  .reset-btn { background: none; border: none; font-size: 13px; color: var(--text-3); padding: 8px 4px; transition: color 0.15s; }
  .reset-btn:hover { color: var(--text); }
  .mono { font-family: var(--mono); }
</style>
