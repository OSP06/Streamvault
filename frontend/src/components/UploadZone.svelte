<script>
  import { createEventDispatcher } from 'svelte';

  const dispatch = createEventDispatcher();

  const MAX_SIZE = 1024 * 1024 * 1024; // 1GB
  const ALLOWED_EXTS = ['mp4','webm','mov','avi','mkv','ts','mpeg','mpg','m4v'];

  let dragOver = false;
  let uploading = false;
  let progress = 0;
  let uploadedResult = null;
  let inputEl;

  function handleDragEnter(e) {
    e.preventDefault();
    dragOver = true;
  }
  function handleDragLeave(e) {
    e.preventDefault();
    if (!e.currentTarget.contains(e.relatedTarget)) dragOver = false;
  }
  function handleDrop(e) {
    e.preventDefault();
    dragOver = false;
    const file = e.dataTransfer.files[0];
    if (file) uploadFile(file);
  }
  function handleInputChange(e) {
    const file = e.target.files[0];
    if (file) uploadFile(file);
  }

  function validateFile(file) {
    const ext = file.name.split('.').pop().toLowerCase();
    if (!ALLOWED_EXTS.includes(ext)) {
      dispatch('error', { message: `Unsupported format (.${ext}). Use: ${ALLOWED_EXTS.join(', ')}` });
      return false;
    }
    if (file.size > MAX_SIZE) {
      dispatch('error', { message: 'File exceeds 1GB limit.' });
      return false;
    }
    return true;
  }

  async function uploadFile(file) {
    if (!validateFile(file)) return;

    uploading = true;
    progress = 0;
    uploadedResult = null;

    const formData = new FormData();
    formData.append('video', file);

    try {
      // Use XMLHttpRequest for real upload progress
      await new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/upload');

        xhr.upload.onprogress = (e) => {
          if (e.lengthComputable) {
            progress = Math.round((e.loaded / e.total) * 100);
          }
        };

        xhr.onload = () => {
          if (xhr.status >= 200 && xhr.status < 300) {
            resolve(JSON.parse(xhr.responseText));
          } else {
            try {
              const err = JSON.parse(xhr.responseText);
              reject(new Error(err.error || 'Upload failed'));
            } catch {
              reject(new Error(`Upload failed (${xhr.status})`));
            }
          }
        };

        xhr.onerror = () => reject(new Error('Network error'));

        xhr.onreadystatechange = () => {
          if (xhr.readyState === 4 && xhr.status >= 200 && xhr.status < 300) {
            const result = JSON.parse(xhr.responseText);
            uploadedResult = result;
            dispatch('success', { result });
          }
        };

        xhr.send(formData);
      });
    } catch (e) {
      dispatch('error', { message: e.message });
    } finally {
      uploading = false;
      if (inputEl) inputEl.value = '';
    }
  }

  function copyLink() {
    if (uploadedResult) {
      navigator.clipboard.writeText(uploadedResult.share_url);
    }
  }

  function reset() {
    uploadedResult = null;
    progress = 0;
  }

  function formatBytes(bytes) {
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
  }
</script>

<div class="upload-wrap">
  {#if uploadedResult}
    <!-- Success state -->
    <div class="success-card">
      <div class="success-icon">✓</div>
      <div class="success-body">
        <h3>Upload complete</h3>
        <p class="filename">{uploadedResult.original_name}</p>
        <p class="filesize mono">{formatBytes(uploadedResult.size_bytes)}</p>
        <div class="link-row">
          <input class="link-input mono" readonly value={uploadedResult.share_url} />
          <button class="copy-btn" on:click={copyLink}>Copy</button>
        </div>
        <div class="action-row">
          <a class="watch-btn" href="/watch/{uploadedResult.token}" target="_blank">
            ▶ Watch Now
          </a>
          <button class="reset-btn" on:click={reset}>Upload another</button>
        </div>
      </div>
    </div>
  {:else if uploading}
    <!-- Upload progress state -->
    <div class="progress-card">
      <div class="progress-header">
        <span>Uploading…</span>
        <span class="mono">{progress}%</span>
      </div>
      <div class="progress-bar-wrap">
        <div class="progress-bar" style="width: {progress}%"></div>
      </div>
      <p class="progress-note">Streaming to server — you can watch immediately after upload.</p>
    </div>
  {:else}
    <!-- Drop zone -->
    <div
      class="dropzone"
      class:drag-active={dragOver}
      on:dragenter={handleDragEnter}
      on:dragover|preventDefault
      on:dragleave={handleDragLeave}
      on:drop={handleDrop}
      role="button"
      tabindex="0"
      on:keydown={e => e.key === 'Enter' && inputEl?.click()}
    >
      <input
        bind:this={inputEl}
        type="file"
        accept="video/*"
        hidden
        on:change={handleInputChange}
      />
      <div class="drop-content">
        <div class="drop-icon">
          <svg width="40" height="40" viewBox="0 0 40 40" fill="none">
            <rect width="40" height="40" rx="8" fill="var(--accent-glow)"/>
            <path d="M20 8 L20 26 M13 19 L20 26 L27 19" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
            <path d="M10 30 L30 30" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" opacity="0.5"/>
          </svg>
        </div>
        <p class="drop-title">Drop your video here</p>
        <p class="drop-sub">or <button class="file-btn" on:click={() => inputEl?.click()}>browse files</button></p>
        <div class="constraints">
          <span class="mono">Max 1GB</span>
          <span class="sep">·</span>
          <span class="mono">{ALLOWED_EXTS.join(' · ')}</span>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .upload-wrap {
    max-width: 680px;
    margin: 0 auto;
  }

  /* Drop zone */
  .dropzone {
    border: 2px dashed var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    padding: 56px 32px;
    text-align: center;
    cursor: pointer;
    transition: border-color 0.2s, background 0.2s;
    outline: none;
  }

  .dropzone:hover, .dropzone:focus {
    border-color: var(--accent-dim);
    background: var(--surface-2);
  }

  .drag-active {
    border-color: var(--accent) !important;
    background: var(--accent-glow) !important;
  }

  .drop-icon {
    margin-bottom: 16px;
    display: flex;
    justify-content: center;
  }

  .drop-title {
    font-size: 17px;
    font-weight: 600;
    color: var(--text);
    margin-bottom: 6px;
    letter-spacing: -0.02em;
  }

  .drop-sub {
    font-size: 14px;
    color: var(--text-2);
    margin-bottom: 20px;
  }

  .file-btn {
    background: none;
    border: none;
    color: var(--accent);
    font-size: 14px;
    padding: 0;
    text-decoration: underline;
    font-family: var(--sans);
  }

  .constraints {
    font-size: 11px;
    color: var(--text-3);
    font-family: var(--mono);
    display: flex;
    gap: 8px;
    justify-content: center;
    flex-wrap: wrap;
  }

  .sep { color: var(--border-bright); }

  /* Progress */
  .progress-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 32px;
  }

  .progress-header {
    display: flex;
    justify-content: space-between;
    font-weight: 500;
    margin-bottom: 12px;
    font-size: 14px;
  }

  .progress-bar-wrap {
    height: 6px;
    background: var(--surface-2);
    border-radius: 3px;
    overflow: hidden;
    margin-bottom: 12px;
  }

  .progress-bar {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.15s ease;
    box-shadow: 0 0 12px var(--accent-glow);
  }

  .progress-note {
    font-size: 12px;
    color: var(--text-3);
    font-family: var(--mono);
  }

  /* Success */
  .success-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 32px;
    display: flex;
    gap: 20px;
    align-items: flex-start;
  }

  .success-icon {
    width: 40px;
    height: 40px;
    background: rgba(34, 197, 94, 0.15);
    color: var(--green);
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    flex-shrink: 0;
  }

  .success-body {
    flex: 1;
    min-width: 0;
  }

  .success-body h3 {
    font-size: 16px;
    font-weight: 600;
    margin-bottom: 4px;
  }

  .filename {
    font-size: 13px;
    color: var(--text-2);
    margin-bottom: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .filesize {
    font-size: 11px;
    color: var(--text-3);
    margin-bottom: 16px;
  }

  .link-row {
    display: flex;
    gap: 8px;
    margin-bottom: 16px;
  }

  .link-input {
    flex: 1;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 8px 12px;
    font-size: 12px;
    color: var(--text-2);
    min-width: 0;
    outline: none;
  }

  .copy-btn {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 8px 14px;
    font-size: 13px;
    color: var(--text);
    white-space: nowrap;
    transition: border-color 0.15s, color 0.15s;
  }

  .copy-btn:hover {
    border-color: var(--accent);
    color: var(--accent);
  }

  .action-row {
    display: flex;
    gap: 10px;
    align-items: center;
  }

  .watch-btn {
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
    padding: 8px 16px;
    font-size: 13px;
    font-weight: 500;
    transition: background 0.15s;
    text-decoration: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .watch-btn:hover {
    background: var(--accent-dim);
    text-decoration: none;
  }

  .reset-btn {
    background: none;
    border: none;
    font-size: 13px;
    color: var(--text-3);
    padding: 8px 4px;
    transition: color 0.15s;
  }

  .reset-btn:hover {
    color: var(--text);
  }

  .mono { font-family: var(--mono); }
</style>
