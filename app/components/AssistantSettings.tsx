import React, { useEffect, useState } from 'react';
import { Check, Download, Loader2, PenLine, Square } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { ChoiceTabs } from './DevicePicker';
import { OptionalGroup } from './SetupGate';
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
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-400 dark:border-white/10 dark:bg-black/20 dark:text-white';

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

      // The provider page owns which model each capability uses, and the
      // assistant reads it from there. Saving here without updating it left two
      // screens naming different models and the request going to the other one.
      if (provider === 'open_router') {
        const configuration = await fetch('/v1/configuration').then(response => response.json()).catch(() => null);
        const selections = configuration?.selections;
        if (Array.isArray(selections)) {
          await fetch('/v1/configuration', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
              selections: selections.map((selection: { capability: string; cloud_model: string | null }) =>
                selection.capability === 'prompt_enhancement'
                  ? { ...selection, execution_mode: 'open_router', cloud_model: openRouterModel.trim() || null }
                  : selection,
              ),
            }),
          }).catch(() => undefined);
        }
      }

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

        <ChoiceTabs
          columns={2}
          options={[
            { id: 'none' as Provider, label: t('assistantDisabled') },
            { id: 'managed' as Provider, label: t('assistantManaged') },
            { id: 'open_router' as Provider, label: 'OpenRouter' },
            { id: 'local' as Provider, label: t('assistantLocal') },
          ]}
          value={provider}
          onChange={setProvider}
        />

        {provider === 'managed' && (
          <div className="space-y-2">
            <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantManagedHint')}</p>
            {/* The one place a capability is set up. This page used to draw
                its own list of eight files, with its own buttons and its own
                idea of progress, beside the same capability on the download
                page - two controllers for one thing, disagreeing. */}
            <OptionalGroup
              title={t('assistantSection')}
              purpose={t('assistantOptionalPurpose')}
              statusUrl="/v1/assistant/runtime"
              installUrl="/v1/assistant/runtime/install"
              settingsUrl="/v1/assistant/status"
              engines={[{ id: 'managed', label: 'llama.cpp' }]}
            />
            <div className="rounded-lg border-2 border-dashed border-zinc-300 p-3 dark:border-white/10">
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
                className="inline-flex items-center gap-2 rounded-lg border border-zinc-200 px-3 py-1.5 text-xs font-medium text-zinc-600 hover:border-pink-400 dark:border-white/10 dark:text-zinc-300"
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
                className="shrink-0 rounded-lg border border-zinc-200 px-3 py-2 text-sm font-medium text-zinc-600 hover:border-pink-400 disabled:opacity-50 dark:border-white/10 dark:text-zinc-300"
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
