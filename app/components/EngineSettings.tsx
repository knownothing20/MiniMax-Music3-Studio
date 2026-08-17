import React, { useCallback, useEffect, useState } from 'react';
import { AlertTriangle, Check, Cpu, Loader2, RotateCw } from 'lucide-react';

/**
 * Launch options for the local engine.
 *
 * These are `mm-server` command-line flags, and upstream reads them once at
 * startup — so saving them restarts the engine. Each one is a real flag, and
 * `--max-batch` in particular is the ceiling on how many songs a single request
 * may render, which is why the create panel clamps to it.
 */

interface EngineOptions {
  keep_loaded: boolean;
  max_batch: number | null;
  max_seq: number | null;
  disable_flash_attention: boolean;
  split_cfg_forwards: boolean;
  clamp_fp16: boolean;
}

const DEFAULTS: EngineOptions = {
  keep_loaded: false,
  max_batch: null,
  max_seq: null,
  disable_flash_attention: false,
  split_cfg_forwards: false,
  clamp_fp16: false,
};

const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-500 dark:border-white/10 dark:bg-black/20 dark:text-white';

const Toggle: React.FC<{ label: string; hint: string; checked: boolean; onChange: (value: boolean) => void }> = ({ label, hint, checked, onChange }) => (
  <label className="flex cursor-pointer items-start gap-3 rounded-xl border border-zinc-200 p-3 dark:border-white/10">
    <input type="checkbox" checked={checked} onChange={event => onChange(event.target.checked)} className="mt-0.5 h-4 w-4 accent-pink-500" />
    <span className="min-w-0">
      <span className="block text-sm font-medium text-zinc-800 dark:text-zinc-100">{label}</span>
      <span className="mt-0.5 block text-xs leading-5 text-zinc-500 dark:text-zinc-400">{hint}</span>
    </span>
  </label>
);

export const EngineSettings: React.FC = () => {
  const [options, setOptions] = useState<EngineOptions>(DEFAULTS);
  const [saved, setSaved] = useState<EngineOptions>(DEFAULTS);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const response = await fetch('/engine/options');
      if (!response.ok) throw new Error(`Engine options are unavailable (${response.status})`);
      const body: { options: EngineOptions } = await response.json();
      setOptions(body.options);
      setSaved(body.options);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Could not read the engine options.');
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(options) !== JSON.stringify(saved);

  const save = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const response = await fetch('/engine/options', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(options),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || `Saving the engine options failed (${response.status})`);
      setSaved(body.options);
      setNotice(body.engine_restarted
        ? 'Saved. The engine was restarted so the new flags take effect.'
        : 'Saved. They apply the next time the engine starts.');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Saving the engine options failed.');
    } finally {
      setBusy(false);
    }
  };

  const restart = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const response = await fetch('/engine/restart', { method: 'POST' });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || `Restarting the engine failed (${response.status})`);
      setNotice('The engine was restarted.');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Restarting the engine failed.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3">
      <h4 className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
        <Cpu size={16} className="text-pink-500" /> Local engine
      </h4>

      <Toggle
        label="Keep models in VRAM between jobs"
        hint="--keep-loaded. Back-to-back generation stops reloading each module, at the cost of a permanently higher VRAM footprint."
        checked={options.keep_loaded}
        onChange={value => setOptions(current => ({ ...current, keep_loaded: value }))}
      />

      <div className="grid gap-3 sm:grid-cols-2">
        <label className="block text-xs font-medium text-zinc-600 dark:text-zinc-300">
          <span className="mb-1.5 block">Songs per request (--max-batch)</span>
          <input
            type="number"
            min={1}
            max={8}
            value={options.max_batch ?? 1}
            onChange={event => {
              const value = Number(event.target.value);
              setOptions(current => ({ ...current, max_batch: Number.isFinite(value) && value > 1 ? Math.min(8, value) : null }));
            }}
            className={CONTROL}
          />
          <span className="mt-1 block font-normal text-zinc-500">The engine rejects a request whose song count exceeds this.</span>
        </label>
        <label className="block text-xs font-medium text-zinc-600 dark:text-zinc-300">
          <span className="mb-1.5 block">LM KV cache (--max-seq)</span>
          <input
            type="number"
            min={512}
            step={512}
            placeholder="model context"
            value={options.max_seq ?? ''}
            onChange={event => {
              const value = Number(event.target.value);
              setOptions(current => ({ ...current, max_seq: event.target.value === '' || !Number.isFinite(value) ? null : value }));
            }}
            className={CONTROL}
          />
          <span className="mt-1 block font-normal text-zinc-500">Empty uses the model's own context length.</span>
        </label>
      </div>

      <details className="rounded-xl border border-zinc-200 p-3 dark:border-white/10">
        <summary className="cursor-pointer text-xs font-semibold uppercase tracking-wide text-zinc-500">Diagnostics</summary>
        <div className="mt-3 space-y-3">
          <Toggle
            label="Disable flash attention"
            hint="--no-fa. Slower; use it when a driver or card misbehaves with flash attention."
            checked={options.disable_flash_attention}
            onChange={value => setOptions(current => ({ ...current, disable_flash_attention: value }))}
          />
          <Toggle
            label="Split CFG into two forwards"
            hint="--no-batch-cfg. Lower peak VRAM during guidance, slower per step."
            checked={options.split_cfg_forwards}
            onChange={value => setOptions(current => ({ ...current, split_cfg_forwards: value }))}
          />
          <Toggle
            label="Clamp hidden states to FP16 range"
            hint="--clamp-fp16. A numerical workaround for overflow artefacts."
            checked={options.clamp_fp16}
            onChange={value => setOptions(current => ({ ...current, clamp_fp16: value }))}
          />
        </div>
      </details>

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={() => void save()}
          disabled={!dirty || busy}
          className="inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-xs font-bold text-white disabled:opacity-50"
        >
          {busy ? <Loader2 size={14} className="animate-spin" /> : null} Save and restart engine
        </button>
        <button
          type="button"
          onClick={() => void restart()}
          disabled={busy}
          className="inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-2 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 disabled:opacity-50 dark:border-white/15 dark:text-zinc-200"
        >
          <RotateCw size={13} /> Restart engine
        </button>
      </div>

      {notice && (
        <p className="flex items-center gap-2 rounded-lg bg-emerald-500/10 px-3 py-2 text-xs text-emerald-700 dark:text-emerald-300">
          <Check size={14} /> {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="flex items-center gap-2 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-300">
          <AlertTriangle size={14} /> {error}
        </p>
      )}
    </div>
  );
};
