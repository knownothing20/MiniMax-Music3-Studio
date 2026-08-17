import React, { useEffect, useState } from 'react';
import { Check, Loader2, PenLine } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { refreshNativeOpenRouterCatalog, type NativeOpenRouterModel } from '../services/nativeOpenRouter';

/**
 * The optional writing assistant.
 *
 * Music3 needs no language model: its own LM emits audio codes, and the form
 * is written by hand. A text model is only useful for drafting the structured
 * caption and lyrics, so this stays off by default and never downloads or
 * starts anything on its own.
 */

type Provider = 'none' | 'local' | 'open_router';

interface AssistantStatus {
  available?: boolean;
  provider?: Provider;
  local_base_url?: string | null;
  local_model?: string | null;
  openrouter_model?: string | null;
}

const INPUT =
  'w-full rounded-lg border-2 border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-indigo-500 dark:border-zinc-700 dark:bg-zinc-800 dark:text-white';

export const AssistantSettings: React.FC = () => {
  const { t } = useI18n();

  const [provider, setProvider] = useState<Provider>('none');
  const [baseUrl, setBaseUrl] = useState('');
  const [localModel, setLocalModel] = useState('');
  const [openRouterModel, setOpenRouterModel] = useState('');
  const [available, setAvailable] = useState(false);
  const [catalog, setCatalog] = useState<NativeOpenRouterModel[]>([]);
  const [busy, setBusy] = useState<'save' | 'catalog' | null>(null);
  const [message, setMessage] = useState<{ tone: 'ok' | 'error'; text: string } | null>(null);

  useEffect(() => {
    void fetch('/v1/assistant/status')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error(String(response.status)))))
      .then((status: AssistantStatus) => {
        setProvider(status.provider ?? 'none');
        setBaseUrl(status.local_base_url ?? '');
        setLocalModel(status.local_model ?? '');
        setOpenRouterModel(status.openrouter_model ?? '');
        setAvailable(status.available === true);
      })
      .catch(() => undefined);
  }, []);

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

        <div className="grid grid-cols-3 gap-2">
          {(['none', 'local', 'open_router'] as const).map(value => (
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
              {value === 'none' ? t('assistantDisabled') : value === 'local' ? t('assistantLocal') : 'OpenRouter'}
            </button>
          ))}
        </div>

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
