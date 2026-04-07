<script>
  export let type = 'success';
  export let message = '';
</script>

<div class="toast toast--{type}" role="alert">
  <span class="icon">{type === 'success' ? '✓' : '✗'}</span>
  <span>{message}</span>
</div>

<style>
  .toast { position: fixed; bottom: 24px; right: 24px; display: flex; align-items: center; gap: 10px; padding: 12px 18px; border-radius: var(--radius); font-size: 13px; font-weight: 500; z-index: 1000; animation: slide-up 0.2s ease; max-width: 360px; box-shadow: 0 4px 24px rgba(0,0,0,0.4); }
  .toast--success { background: rgba(34,197,94,.15); border: 1px solid rgba(34,197,94,.3); color: var(--green); }
  .toast--error   { background: rgba(239,68,68,.15);  border: 1px solid rgba(239,68,68,.3);  color: var(--red); }
  .icon { font-size: 15px; flex-shrink: 0; }
  @keyframes slide-up { from { transform: translateY(12px); opacity: 0; } to { transform: translateY(0); opacity: 1; } }
</style>
