import React, { useEffect, useState } from 'react';
import { Check, Download, Loader2, PenLine, Square } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { loadNativeOpenRouterCatalog, refreshNativeOpenRouterCatalog, type NativeOpenRouterModel } from '../services/nativeOpenRouter';

/**
 * The optional writing assistant.
 *
 * Music3 needs no language model: its own LM emits audio codes, and the form
 * is written by hand. A text model is only useful for drafting the structured
 * caption and lyrics, so this stays off by default and never downloads or
 * starts anything on its own.
 */

type Provider = 'none' | 'local' | 'open_router' | 'managed';

interface RuntimeAsset {
  id: string;
  label: string;
  kind: 'model' | 'runtime';
  bytes: number;
  vram_gb?: number | null;
  note: string;
  installed: boolean;
  downloaded_bytes: number;
}

interface RuntimeStatus {
  ready: boolean;
  root: string;
  server_path?: string | null;
  installed_models: string[];
  running_model?: string | null;
  assets: RuntimeAsset[];
  active_download?: { asset_id: string; downloaded_bytes: number; total_bytes: number; done: boolean; error?: string | null } | null;
}

/// The service ships one English note per asset; these are its translations.
const ASSET_NOTE: Record<string, string> = {
  'gemma-4-e4b-q4_0': 'assetNoteGemmaE4b',
  'gemma-4-12b-q4_0': 'assetNoteGemma12b',
  'llama-cuda': 'assetNoteLlamaCuda',
  'llama-cuda-runtime': 'assetNoteCudart',
  'llama-cpu': 'assetNoteLlamaCpu',
};

const gigabytes = (bytes: number) => `${(bytes / 1e9).toFixed(bytes < 1e9 ? 2 : 1)} GB`;

interface AssistantStatus {
  available?: boolean;
  managed_path?: string | null;
  provider?: Provider;
  local_base_url?: string | null;
  local_model?: string | null;
  openrouter_model?: string | null;
  managed_model?: string | null;
}

const INPUT =
  'w-full rounded-lg border-2 border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-indigo-500 dark:border-zinc-700 dark:bg-zinc-800 dark:text-white';

export const AssistantSettings: React.FC = () => {
  const { t } = useI18n();

  const [provider, setProvider] = useState<Provider>('none');
  const [baseUrl, setBaseUrl] = useState('');
  const [localModel, setLocalModel] = useState('');
  const [openRouterModel, setOpenRouterModel] = useState('');
  const [managedModel, setManagedModel] = useState('');
  const [managedPath, setManagedPath] = useState('');
  const [runtime, setRuntime] = useState<RuntimeStatus | null>(null);
  const [available, setAvailable] = useState(false);
  const [catalog, setCatalog] = useState<NativeOpenRouterModel[]>([]);
  const [busy, setBusy] = useState<'save' | 'catalog' | null>(null);
  const [message, setMessage] = useState<{ tone: 'ok' | 'error'; text: string } | null>(null);

  // The catalog is already there; asking the user to press refresh to see it
  // is the program making them do its work.
  useEffect(() => {
    void loadNativeOpenRouterCatalog()
      .then((models) => setCatalog(models.filter((model) => model.capabilities.includes('prompt_enhancement'))))
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    void fetch('/v1/assistant/status')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error(String(response.status)))))
      .then((status: AssistantStatus) => {
        setProvider(status.provider ?? 'none');
        setBaseUrl(status.local_base_url ?? '');
        setLocalModel(status.local_model ?? '');
        setOpenRouterModel(status.openrouter_model ?? '');
        setManagedModel(status.managed_model ?? '');
        setManagedPath(status.managed_path ?? '');
        setAvailable(status.available === true);
      })
      .catch(() => undefined);
  }, []);

  const loadRuntime = React.useCallback(async () => {
    const response = await fetch('/v1/assistant/runtime');
    if (response.ok) setRuntime(await response.json());
  }, []);

  useEffect(() => { void loadRuntime().catch(() => undefined); }, [loadRuntime]);

  // While something is downloading, keep the figures moving.
  useEffect(() => {
    const active = runtime?.active_download;
    if (!active || active.done) return;
    const timer = window.setInterval(() => void loadRuntime().catch(() => undefined), 1500);
    return () => window.clearInterval(timer);
  }, [runtime?.active_download, loadRuntime]);

  const install = async (assetId: string) => {
    setMessage(null);
    try {
      const response = await fetch('/v1/assistant/runtime/install', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ asset_id: assetId }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || String(response.status));
      setRuntime(body);
    } catch (reason) {
      setMessage({ tone: 'error', text: reason instanceof Error ? reason.message : String(reason) });
    }
  };

  const stopSidecar = async () => {
    await fetch('/v1/assistant/runtime/stop', { method: 'POST' }).catch(() => undefined);
    void loadRuntime().catch(() => undefined);
  };

  const loadCatalog = async () => {
    setBusy('catalog');
    setMessage(null);
    try {
      const models = await refreshNativeOpenRouterCatalog();
      setCatalog(models.filter(model => model.capabilities.includes('prompt_enhancement')));
    } catch (reason) {
      setMessage({ tone: 'error', text: reason instanceof Error ? reason.message : String(reason) });
    } finally {
      setBusy(null);
    }
  };

  const save = async () => {
    setBusy('save');
    setMessage(null);
    try {
      const response = await fetch('/v1/assistant/status', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          provider,
          local_base_url: baseUrl.trim() || null,
          local_model: localModel.trim() || null,
          openrouter_model: openRouterModel.trim() || null,
          managed_model: managedModel.trim() || null,
          managed_path: managedPath.trim() || null,
        }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || String(response.status));
      setAvailable(body?.available === true);
      setMessage({ tone: 'ok', text: t('assistantSaved') });
    } catch (reason) {
      setMessage({ tone: 'error', text: reason instanceof Error ? reason.message : String(reason) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2 text-zinc-900 dark:text-white">
        <PenLine size={20} />
        <h3 className="font-semibold">{t('assistantSection')}</h3>
        <span className={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${available ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-zinc-200 text-zinc-500 dark:bg-white/10 dark:text-zinc-400'}`}>
          {available ? t('assistantOn') : t('assistantOff')}
        </span>
      </div>

      <div className="space-y-3 pl-7">
        <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantHint')}</p>

        <div className="grid grid-cols-2 gap-2">
          {(['none', 'managed', 'open_router', 'local'] as const).map(value => (
            <button
              key={value}
              type="button"
              onClick={() => setProvider(value)}
              className={`rounded-lg border-2 py-2 text-sm font-medium transition-colors ${
                provider === value
                  ? 'border-indigo-500 bg-indigo-50 text-indigo-700 dark:bg-indigo-950 dark:text-indigo-300'
                  : 'border-zinc-300 text-zinc-600 hover:border-zinc-400 dark:border-zinc-700 dark:text-zinc-300 dark:hover:border-zinc-600'
              }`}
            >
              {value === 'none'
                ? t('assistantDisabled')
                : value === 'managed'
                  ? t('assistantManaged')
                  : value === 'local'
                    ? t('assistantLocal')
                    : 'OpenRouter'}
            </button>
          ))}
        </div>

        {provider === 'managed' && (
          <div className="space-y-2">
            <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantManagedHint')}</p>
            {runtime?.assets.map(asset => {
              const active = runtime.active_download?.asset_id === asset.id && !runtime.active_download?.done;
              const percent = active && runtime.active_download
                ? Math.min(100, Math.round((runtime.active_download.downloaded_bytes / Math.max(1, runtime.active_download.total_bytes)) * 100))
                : 0;
              return (
                <div key={asset.id} className="rounded-lg border-2 border-zinc-300 p-3 dark:border-zinc-700">
                  <div className="flex items-center justify-between gap-3">
                    <label className="flex min-w-0 items-center gap-2">
                      {asset.kind === 'model' && (
                        <input
                          type="radio"
                          name="managed-model"
                          checked={managedModel === asset.id}
                          onChange={() => setManagedModel(asset.id)}
                          disabled={!asset.installed}
                          className="h-4 w-4 accent-indigo-500"
                        />
                      )}
                      <span className="truncate text-sm font-medium text-zinc-900 dark:text-white">{asset.label}</span>
                    </label>
                    <div className="flex shrink-0 items-center gap-2">
                      <span className="text-xs tabular-nums text-zinc-500">{gigabytes(asset.bytes)}</span>
                      {asset.installed ? (
                        <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 text-[10px] font-semibold text-emerald-600 dark:text-emerald-300">
                          {t('installed')}
                        </span>
                      ) : (
                        <button
                          type="button"
                          onClick={() => void install(asset.id)}
                          disabled={Boolean(runtime.active_download && !runtime.active_download.done)}
                          className="inline-flex items-center gap-1 rounded-lg border-2 border-zinc-300 px-2 py-1 text-xs font-medium text-zinc-600 hover:border-zinc-400 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300"
                        >
                          {active ? <Loader2 size={13} className="animate-spin" /> : <Download size={13} />}
                          {active ? `${percent}%` : t('download')}
                        </button>
                      )}
                    </div>
                  </div>
                  <p className="mt-1 text-[11px] leading-4 text-zinc-500">{ASSET_NOTE[asset.id] ? t(ASSET_NOTE[asset.id] as never) : asset.note}</p>
                  {active && (
                    <div className="mt-2 h-1 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-700">
                      <div className="h-full bg-indigo-500 transition-[width]" style={{ width: `${percent}%` }} />
                    </div>
                  )}
                </div>
              );
            })}
            <div className="rounded-lg border-2 border-dashed border-zinc-300 p-3 dark:border-zinc-700">
              <label className="block text-xs font-medium text-zinc-500 dark:text-zinc-400">{t('assistantOwnFile')}</label>
              <input
                value={managedPath}
                onChange={event => setManagedPath(event.target.value)}
                placeholder="D:\models\gemma-4-12b-it-qat-q4_0.gguf"
                className={`${INPUT} mt-1`}
              />
              <p className="mt-1 text-[11px] leading-4 text-zinc-500">{t('assistantOwnFileHint')}</p>
            </div>
            {runtime?.active_download?.error && (
              <p className="text-xs text-red-600 dark:text-red-300">{runtime.active_download.error}</p>
            )}
            {runtime?.running_model && (
              <button
                type="button"
                onClick={() => void stopSidecar()}
                className="inline-flex items-center gap-2 rounded-lg border-2 border-zinc-300 px-3 py-1.5 text-xs font-medium text-zinc-600 hover:border-zinc-400 dark:border-zinc-700 dark:text-zinc-300"
              >
                <Square size={13} /> {t('assistantUnload')}
              </button>
            )}
          </div>
        )}

        {provider === 'local' && (
          <div className="space-y-2">
            <label className="block text-xs font-medium text-zinc-500 dark:text-zinc-400">{t('assistantBaseUrl')}</label>
            <input value={baseUrl} onChange={event => setBaseUrl(event.target.value)} placeholder="http://127.0.0.1:8080/v1" className={INPUT} />
            <label className="block text-xs font-medium text-zinc-500 dark:text-zinc-400">{t('assistantModel')}</label>
            <input value={localModel} onChange={event => setLocalModel(event.target.value)} placeholder="gemma-3-4b-it" className={INPUT} />
            <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantLocalHint')}</p>
          </div>
        )}

        {provider === 'open_router' && (
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <select value={openRouterModel} onChange={event => setOpenRouterModel(event.target.value)} className={INPUT}>
                <option value="">{t('assistantPickModel')}</option>
                {openRouterModel && !catalog.some(model => model.id === openRouterModel) && (
                  <option value={openRouterModel}>{openRouterModel}</option>
                )}
                {catalog.map(model => <option key={model.id} value={model.id}>{model.name}</option>)}
              </select>
              <button
                type="button"
                onClick={() => void loadCatalog()}
                disabled={busy !== null}
                className="shrink-0 rounded-lg border-2 border-zinc-300 px-3 py-2 text-sm font-medium text-zinc-600 hover:border-zinc-400 disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-300"
              >
                {busy === 'catalog' ? <Loader2 size={16} className="animate-spin" /> : t('refresh')}
              </button>
            </div>
            <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantOpenRouterHint')}</p>
          </div>
        )}

        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => void save()}
            disabled={busy !== null}
            className="inline-flex items-center gap-2 rounded-lg bg-zinc-900 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-zinc-800 disabled:opacity-50 dark:bg-white dark:text-black dark:hover:bg-zinc-200"
          >
            {busy === 'save' ? <Loader2 size={16} className="animate-spin" /> : <Check size={16} />}
            {t('save')}
          </button>
          {message && (
            <span className={`text-xs ${message.tone === 'ok' ? 'text-emerald-600 dark:text-emerald-300' : 'text-red-600 dark:text-red-300'}`}>
              {message.text}
            </span>
          )}
        </div>
      </div>
    </div>
  );
};
