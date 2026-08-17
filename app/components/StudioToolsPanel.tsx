import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Boxes, Check, Copy, FileAudio, FolderOpen, Loader2, RefreshCw, RotateCw, ShieldCheck, Terminal } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { transcribeWithNativeOpenRouter } from '../services/nativeOpenRouter';

/**
 * Studio tools.
 *
 * Diagnostics for the local runtime plus the one cloud utility that has a real
 * place here: turning an audio file into text you can paste into a lyric sheet.
 * Cover art deliberately lives with the track it belongs to, not in a general
 * tools page.
 */

interface SetupStatus {
  ready?: boolean;
  engine_ready?: boolean;
  engine_id?: string;
  model_root?: string;
  selected_profile_id?: string | null;
  selected_component_ids?: string[] | null;
  hardware?: { gpuName?: string; totalVramGb?: number; reason?: string };
}

interface EngineDescriptor {
  id?: string;
  display_name?: string;
  execution_mode?: string;
  installed?: boolean;
  capabilities?: string[];
}

interface CatalogModel {
  id: string;
  name: string;
  capabilities: string[];
}

const CARD = 'rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/[0.03]';
const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-500 disabled:opacity-50 dark:border-white/10 dark:bg-black/30 dark:text-white';

export function StudioToolsPanel(): React.ReactElement {
  const { t } = useI18n();
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [engines, setEngines] = useState<EngineDescriptor[]>([]);
  const [loading, setLoading] = useState(true);
  const [restarting, setRestarting] = useState(false);

  const [logs, setLogs] = useState<string[]>([]);
  const [logsOpen, setLogsOpen] = useState(false);

  const [asrModels, setAsrModels] = useState<CatalogModel[]>([]);
  const [asrModel, setAsrModel] = useState('');
  const [asrFile, setAsrFile] = useState<File | null>(null);
  const [asrLanguage, setAsrLanguage] = useState('');
  const [asrBusy, setAsrBusy] = useState(false);
  const [asrText, setAsrText] = useState('');
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const filePicker = useRef<HTMLInputElement | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [statusResponse, capabilitiesResponse, catalogResponse] = await Promise.all([
        fetch('/setup/status'),
        fetch('/v1/capabilities'),
        fetch('/v1/openrouter/catalog'),
      ]);
      setSetup(statusResponse.ok ? await statusResponse.json() : null);
      setEngines(capabilitiesResponse.ok ? (await capabilitiesResponse.json()).engines ?? [] : []);
      if (catalogResponse.ok) {
        const models: CatalogModel[] = (await catalogResponse.json()).models ?? [];
        const speech = models.filter(model => model.capabilities.includes('speech_to_text'));
        setAsrModels(speech);
        setAsrModel(current => current || speech[0]?.id || '');
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);

  useEffect(() => {
    if (!logsOpen) return;
    const poll = () => void fetch('/v1/engine/logs')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error(String(response.status)))))
      .then((body: { lines?: string[] }) => setLogs((body.lines ?? []).slice(-200)))
      .catch(() => setLogs([t('engineLogsUnavailable')]));
    poll();
    const timer = window.setInterval(poll, 2500);
    return () => window.clearInterval(timer);
  }, [logsOpen, t]);

  const restartEngine = async () => {
    setRestarting(true);
    setError(null);
    try {
      const response = await fetch('/engine/restart', { method: 'POST' });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || `${response.status}`);
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setRestarting(false);
    }
  };

  const transcribe = async () => {
    if (!asrFile || !asrModel) return;
    setAsrBusy(true);
    setError(null);
    setAsrText('');
    try {
      setAsrText(await transcribeWithNativeOpenRouter(asrModel, asrFile, asrLanguage.trim() || undefined));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setAsrBusy(false);
    }
  };

  const copyText = async () => {
    try {
      await navigator.clipboard.writeText(asrText);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard permission is not worth an error banner.
    }
  };

  const runtimeLine = !setup
    ? t('runtimeUnavailable')
    : setup.ready
      ? `${setup.engine_ready ? t('runtimeRunning') : t('runtimeInstalledNotStarted')} · ${
          setup.selected_component_ids?.length ? t('customSet') : setup.selected_profile_id ?? '-'
        }`
      : t('runtimeNotInstalled');

  return (
    <div className="flex-1 overflow-y-auto bg-white px-5 py-6 dark:bg-suno md:px-8">
      <div className="mx-auto max-w-6xl space-y-6">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.18em] text-pink-500">{t('studioTools')}</p>
            <h1 className="mt-1 text-2xl font-bold text-zinc-950 dark:text-white">{t('toolsHeading')}</h1>
            <p className="mt-2 max-w-3xl text-sm text-zinc-600 dark:text-zinc-400">{t('toolsIntro')}</p>
          </div>
          <button
            onClick={() => void refresh()}
            className="inline-flex items-center gap-2 rounded-lg border border-zinc-200 px-3 py-2 text-sm text-zinc-700 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-200"
          >
            <RefreshCw size={15} className={loading ? 'animate-spin' : ''} /> {t('refresh')}
          </button>
        </div>

        <div className="grid gap-4 md:grid-cols-2">
          <section className={CARD}>
            <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
              <ShieldCheck size={17} className="text-pink-500" /> {t('nativeRuntime')}
            </div>
            <p className="mt-3 text-sm text-zinc-600 dark:text-zinc-300">{runtimeLine}</p>
            {setup?.hardware?.reason && <p className="mt-1 text-xs text-zinc-500">{setup.hardware.reason}</p>}
            {setup?.model_root && (
              <p className="mt-3 flex items-start gap-2 break-all text-xs text-zinc-500">
                <FolderOpen size={13} className="mt-0.5 shrink-0" /> {setup.model_root}
              </p>
            )}
            <button
              onClick={() => void restartEngine()}
              disabled={restarting}
              className="mt-4 inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-2 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 disabled:opacity-50 dark:border-white/15 dark:text-zinc-200"
            >
              {restarting ? <Loader2 size={13} className="animate-spin" /> : <RotateCw size={13} />} {t('restartEngine')}
            </button>
          </section>

          <section className={CARD}>
            <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
              <Boxes size={17} className="text-pink-500" /> {t('providerCapabilities')}
            </div>
            <ul className="mt-3 space-y-2">
              {engines.length === 0 && <li className="text-sm text-zinc-500">{t('noCapabilityCatalog')}</li>}
              {engines.map(engine => (
                <li key={engine.id} className="flex flex-wrap items-center gap-2 text-xs">
                  <span className="font-medium text-zinc-800 dark:text-zinc-100">{engine.display_name || engine.id}</span>
                  {engine.installed === false && (
                    <span className="rounded bg-zinc-500/10 px-1.5 py-0.5 text-[10px] text-zinc-500">{t('notInstalledSuffix')}</span>
                  )}
                  <span className="text-zinc-500">{(engine.capabilities || []).join(', ')}</span>
                </li>
              ))}
            </ul>
          </section>
        </div>

        <section className={CARD}>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
              <FileAudio size={17} className="text-pink-500" /> {t('transcribeAudio')}
            </div>
            <span className="text-xs text-zinc-500">{asrModels.length > 0 ? `${asrModels.length}` : t('catalogNotLoaded')}</span>
          </div>
          <p className="mt-2 max-w-3xl text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('transcribeHint')}</p>

          <div className="mt-3 grid gap-3 md:grid-cols-3">
            <select value={asrModel} onChange={event => setAsrModel(event.target.value)} disabled={asrModels.length === 0} className={CONTROL}>
              {asrModels.length === 0 && <option value="">{t('catalogNotLoaded')}</option>}
              {asrModels.map(model => <option key={model.id} value={model.id}>{model.name}</option>)}
            </select>

            <button
              type="button"
              onClick={() => filePicker.current?.click()}
              className={`${CONTROL} truncate text-left ${asrFile ? '' : 'text-zinc-400'}`}
            >
              {asrFile ? asrFile.name : t('chooseAudioFile')}
            </button>
            <input
              ref={filePicker}
              type="file"
              accept="audio/*"
              className="hidden"
              onChange={event => { setAsrFile(event.target.files?.[0] ?? null); event.target.value = ''; }}
            />

            <input
              value={asrLanguage}
              onChange={event => setAsrLanguage(event.target.value)}
              placeholder={t('languageOptional')}
              className={CONTROL}
            />
          </div>

          <button
            type="button"
            onClick={() => void transcribe()}
            disabled={!asrFile || !asrModel || asrBusy}
            className="mt-3 inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:cursor-not-allowed disabled:opacity-50"
          >
            {asrBusy ? <Loader2 size={14} className="animate-spin" /> : <FileAudio size={14} />} {t('transcribeAudio')}
          </button>

          {asrText && (
            <div className="mt-3">
              <textarea readOnly value={asrText} rows={6} className={`${CONTROL} resize-y font-mono text-xs`} />
              <button
                type="button"
                onClick={() => void copyText()}
                className="mt-2 inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-1.5 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 dark:border-white/15 dark:text-zinc-200"
              >
                {copied ? <Check size={13} className="text-emerald-500" /> : <Copy size={13} />} {t('copyToClipboard')}
              </button>
            </div>
          )}
        </section>

        <section className={CARD}>
          <button
            type="button"
            onClick={() => setLogsOpen(value => !value)}
            className="flex w-full items-center justify-between text-sm font-semibold text-zinc-900 dark:text-white"
          >
            <span className="flex items-center gap-2">
              <Terminal size={17} className="text-pink-500" /> {t('engineLog')}
            </span>
            <span className="text-xs text-zinc-500">{logsOpen ? '−' : '+'}</span>
          </button>
          {logsOpen && (
            <pre className="mt-3 max-h-80 overflow-auto rounded-xl bg-zinc-950 p-3 text-[10px] leading-4 text-zinc-300">
              {logs.length ? logs.join('\n') : t('noEngineOutput')}
            </pre>
          )}
        </section>

        {error && (
          <p role="alert" className="rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-300">{error}</p>
        )}
      </div>
    </div>
  );
}
