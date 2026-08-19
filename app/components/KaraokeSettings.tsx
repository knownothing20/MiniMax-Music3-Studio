import React, { useCallback, useEffect, useState } from 'react';
import { Check, Download, Loader2, Mic2 } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { ChoiceTabs, DevicePicker } from './DevicePicker';
import { OptionalGroup } from './SetupGate';
import { loadNativeOpenRouterCatalog, refreshNativeOpenRouterCatalog, type NativeOpenRouterModel } from '../services/nativeOpenRouter';

/**
 * Karaoke timings.
 *
 * Off by default, like every optional extra here. When on, one of three
 * recognisers supplies the clock - Parakeet and Whisper run on this machine,
 * OpenRouter runs on the user's own key - and the track's own written lyrics
 * are put on top of it.
 */

type Provider = 'none' | 'whisper' | 'parakeet' | 'open_router';

interface Asset {
  id: string;
  label: string;
  kind: 'model' | 'runtime';
  bytes: number;
  note: string;
  installed: boolean;
  downloaded_bytes: number;
}

interface KaraokeStatus {
  enabled: boolean;
  provider: Provider;
  ready: boolean;
  whisper_binary?: string | null;
  whisper_model?: string | null;
  openrouter_model?: string | null;
  runtime?: Device;
  installed_models: string[];
  assets: Asset[];
  active_download?: { asset_id: string; downloaded_bytes: number; total_bytes: number; done: boolean; error?: string | null } | null;
}

const INPUT =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-400 dark:border-white/10 dark:bg-black/20 dark:text-white';

const megabytes = (bytes: number) => (bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`);

/// What a recogniser is made of. The panel never shows these: the whole set is
/// installed and removed by one button, because a runtime and five model files
/// are not five decisions - they are one recogniser.
const SET_OF: Record<Provider, string[]> = {
  none: [],
  whisper: ['whisper-cuda', 'whisper-cpu'],
  parakeet: ['onnxruntime', 'onnxruntime-cuda', 'cuda-cudart', 'cuda-cublas', 'cuda-cufft', 'cuda-cudnn', 'parakeet-tdt-int8', 'parakeet-decoder', 'parakeet-features', 'parakeet-vocab', 'parakeet-config'],
  open_router: [],
};

type Device = 'auto' | 'cuda' | 'cpu';

export const KaraokeSettings: React.FC = () => {
  const { t } = useI18n();

  const [status, setStatus] = useState<KaraokeStatus | null>(null);
  const [enabled, setEnabled] = useState(false);
  const [provider, setProvider] = useState<Provider>('none');
  const [whisperModel, setWhisperModel] = useState('');
  const [openRouterModel, setOpenRouterModel] = useState('');
  const [device, setDevice] = useState<Device>('auto');
  const [catalog, setCatalog] = useState<NativeOpenRouterModel[]>([]);
  const [busy, setBusy] = useState<'save' | 'catalog' | null>(null);
  const [message, setMessage] = useState<{ tone: 'ok' | 'error'; text: string } | null>(null);

  const load = useCallback(async () => {
    const response = await fetch('/v1/karaoke/status');
    if (!response.ok) return;
    const body: KaraokeStatus = await response.json();
    setStatus(body);
    return body;
  }, []);

  useEffect(() => {
    void loadNativeOpenRouterCatalog()
      .then((models) => setCatalog(models.filter((model) => model.capabilities.includes('speech_to_text'))))
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    void load().then(body => {
      if (!body) return;
      setEnabled(body.enabled);
      setProvider(body.provider);
      setWhisperModel(body.whisper_model ?? '');
      setOpenRouterModel(body.openrouter_model ?? '');
      setDevice(body.runtime ?? 'auto');
    }).catch(() => undefined);
  }, [load]);

  // Keep the figures moving while something is downloading.
  useEffect(() => {
    const active = status?.active_download;
    if (!active || active.done) return;
    const timer = window.setInterval(() => void load().catch(() => undefined), 1500);
    return () => window.clearInterval(timer);
  }, [status?.active_download, load]);


  const remove = async (setId: string) => {
    setMessage(null);
    try {
      const response = await fetch('/v1/karaoke/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ asset_id: setId }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || String(response.status));
      setStatus(body);
    } catch (reason) {
      setMessage({ tone: 'error', text: reason instanceof Error ? reason.message : String(reason) });
    }
  };

  const save = async () => {
    setBusy('save');
    setMessage(null);
    try {
      const response = await fetch('/v1/karaoke/status', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          enabled,
          provider,
          whisper_model: whisperModel.trim() || null,
          openrouter_model: openRouterModel.trim() || null,
          runtime: device,
        }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || String(response.status));
      setStatus(body);
      setMessage({ tone: 'ok', text: t('assistantSaved') });
    } catch (reason) {
      setMessage({ tone: 'error', text: reason instanceof Error ? reason.message : String(reason) });
    } finally {
      setBusy(null);
    }
  };

  const loadCatalog = async () => {
    setBusy('catalog');
    setMessage(null);
    try {
      const models = await refreshNativeOpenRouterCatalog();
      setCatalog(models.filter(model => model.capabilities.includes('speech_to_text')));
    } catch (reason) {
      setMessage({ tone: 'error', text: reason instanceof Error ? reason.message : String(reason) });
    } finally {
      setBusy(null);
    }
  };

  const relevant: Asset[] = [];
  const whisperModels = (status?.assets ?? []).filter(asset => asset.id.startsWith('whisper-') && asset.kind === 'model');

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2 text-zinc-900 dark:text-white">
        <Mic2 size={20} />
        <h3 className="font-semibold">{t('karaokeSection')}</h3>
        <span className={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${status?.ready ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-zinc-200 text-zinc-500 dark:bg-white/10 dark:text-zinc-400'}`}>
          {status?.ready ? t('assistantOn') : t('assistantOff')}
        </span>
      </div>

      <div className="space-y-3 pl-7">
        <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('karaokeHint')}</p>

        <label className="flex cursor-pointer items-center gap-2 text-sm font-medium text-zinc-700 dark:text-zinc-200">
          <input type="checkbox" checked={enabled} onChange={event => setEnabled(event.target.checked)} className="h-4 w-4 accent-pink-500" />
          {t('karaokeEnable')}
        </label>

        {enabled && (
          <>
            <ChoiceTabs
              options={[
                { id: 'parakeet' as Provider, label: 'Parakeet' },
                { id: 'whisper' as Provider, label: 'Whisper' },
                { id: 'open_router' as Provider, label: 'OpenRouter' },
              ]}
              value={provider}
              onChange={setProvider}
            />

            <DevicePicker value={device} onChange={setDevice} />

            {provider === 'whisper' && (
              <div className="space-y-2">
                <label className="block text-xs font-medium text-zinc-500 dark:text-zinc-400">{t('karaokeWhisperModel')}</label>
                <select value={whisperModel} onChange={event => setWhisperModel(event.target.value)} className={INPUT}>
                  <option value="">{t('assistantPickModel')}</option>
                  {whisperModels.map(model => (
                    <option key={model.id} value={model.id} disabled={!model.installed}>
                      {model.label}{model.installed ? '' : ` — ${t('download')} ${megabytes(model.bytes)}`}
                    </option>
                  ))}
                </select>
                <p className="text-[11px] leading-4 text-zinc-500">{t('karaokeRuntimeHint')}</p>
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
                <label className="block text-xs font-medium text-zinc-500 dark:text-zinc-400">{t('karaokeOpenRouterManual')}</label>
                <input
                  value={openRouterModel}
                  onChange={event => setOpenRouterModel(event.target.value)}
                  placeholder="openai/whisper-large-v3"
                  className={INPUT}
                />
                <p className="text-[11px] leading-4 text-zinc-500">{t('karaokeOpenRouterHint')}</p>
              </div>
            )}

            {/* One place installs a recogniser. This page drew its own
                list of files beside the same recogniser on the download page,
                each with its own buttons and its own progress. */}
            <OptionalGroup
              title={t('karaokeSection')}
              purpose={t('karaokeOptionalPurpose')}
              statusUrl="/v1/karaoke/status"
              installUrl="/v1/karaoke/install"
              removeUrl="/v1/karaoke/remove"
              settingsUrl="/v1/karaoke/status"
              engines={[
                { id: 'parakeet', label: 'Parakeet' },
                { id: 'whisper', label: 'Whisper' },
                { id: 'open_router', label: 'OpenRouter', device: false },
              ]}
            />

            {status?.active_download?.error && (
              <p className="text-xs text-red-600 dark:text-red-300">{status.active_download.error}</p>
            )}
          </>
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
