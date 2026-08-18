import React, { useCallback, useEffect, useState } from 'react';
import { AlertTriangle, Download, Loader2, Play, Scissors, X } from 'lucide-react';
import { Song } from '../types';
import { apiUrl } from '../services/apiBase';
import { useI18n } from '../context/I18nContext';

/**
 * Splitting one track into stems.
 *
 * ACE-Step Studio opened a separate page and ran the model in the browser; here
 * the studio's own service does it, on the ONNX Runtime it already carries for
 * karaoke. Nothing is downloaded until the user presses the button: the model
 * is 136 MB and a studio that never separates anything never spends it.
 */

interface StemsModalProps {
  song: Song;
  onClose: () => void;
}

interface SeparationStatus {
  model: { label: string; bytes: number; installed: boolean; note: string };
  runtime_installed: boolean;
  ready: boolean;
  stems: string[];
  download: { downloaded_bytes: number; total_bytes: number; done: boolean } | null;
  run: { song_id: string; progress: number; done: boolean; error: string | null; stems: string[] } | null;
}

const megabytes = (bytes: number) => `${(bytes / 1024 / 1024).toFixed(0)} MB`;

export const StemsModal: React.FC<StemsModalProps> = ({ song, onClose }) => {
  const { t } = useI18n();
  const [status, setStatus] = useState<SeparationStatus | null>(null);
  const [present, setPresent] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    const [separation, stems] = await Promise.all([
      fetch('/v1/separation/status').then(response => response.json()),
      fetch(`/v1/library/songs/${encodeURIComponent(song.id)}/stems`).then(response => response.json()),
    ]);
    setStatus(separation);
    setPresent(stems.stems ?? []);
  }, [song.id]);

  useEffect(() => {
    void refresh().catch(() => undefined);
    const timer = window.setInterval(() => void refresh().catch(() => undefined), 1000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const install = async () => {
    setBusy(true);
    setError(null);
    await fetch('/v1/separation/install', { method: 'POST' }).catch(() => undefined);
    setBusy(false);
  };

  const start = async () => {
    setBusy(true);
    setError(null);
    const response = await fetch(`/v1/library/songs/${encodeURIComponent(song.id)}/stems`, { method: 'POST' });
    if (!response.ok) {
      const body = await response.json().catch(() => null);
      setError(body?.error || `Separation could not start (${response.status})`);
    }
    setBusy(false);
  };

  const download = status?.download;
  const run = status?.run?.song_id === song.id ? status?.run : null;
  const running = Boolean(run && !run.done);
  const percent = Math.round((run?.progress ?? 0) * 100);

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/60 p-4" onClick={onClose}>
      <div className="w-full max-w-lg overflow-hidden rounded-2xl bg-white shadow-2xl dark:bg-zinc-900" onClick={event => event.stopPropagation()}>
        <div className="flex items-center justify-between border-b border-zinc-200 px-5 py-4 dark:border-white/10">
          <h3 className="flex items-center gap-2 text-base font-bold text-zinc-900 dark:text-white">
            <Scissors size={18} className="text-pink-500" /> {t('stemsTitle')}
          </h3>
          <button type="button" onClick={onClose} className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200">
            <X size={18} />
          </button>
        </div>

        <div className="space-y-4 p-5">
          <div>
            <p className="truncate text-sm font-semibold text-zinc-900 dark:text-white">{song.title}</p>
            <p className="mt-1 text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('stemsHint')}</p>
          </div>

          {status && !status.model.installed && (
            <div className="rounded-xl border border-zinc-200 p-3 dark:border-white/10">
              <p className="text-xs leading-5 text-zinc-600 dark:text-zinc-300">
                {status.model.label} · {megabytes(status.model.bytes)}
              </p>
              {download && !download.done ? (
                <div className="mt-2">
                  <div className="h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-black/30">
                    <div
                      className="h-full bg-gradient-to-r from-orange-500 to-pink-500 transition-[width]"
                      style={{ width: `${Math.round((download.downloaded_bytes / Math.max(1, download.total_bytes)) * 100)}%` }}
                    />
                  </div>
                  <p className="mt-1 text-[11px] tabular-nums text-zinc-500">
                    {megabytes(download.downloaded_bytes)} / {megabytes(download.total_bytes)}
                  </p>
                </div>
              ) : (
                <button
                  type="button"
                  onClick={() => void install()}
                  disabled={busy}
                  className="mt-2 inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-xs font-bold text-white disabled:opacity-50"
                >
                  <Download size={14} /> {t('stemsInstallModel')}
                </button>
              )}
            </div>
          )}

          {running && (
            <div className="rounded-xl border border-zinc-200 p-3 dark:border-white/10">
              <div className="flex items-center justify-between text-xs font-medium text-zinc-700 dark:text-zinc-200">
                <span className="flex items-center gap-2"><Loader2 size={14} className="animate-spin text-pink-500" />{t('stemsRunning')}</span>
                <span className="tabular-nums text-zinc-500">{percent}%</span>
              </div>
              <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-black/30">
                <div className="h-full bg-gradient-to-r from-orange-500 to-pink-500 transition-[width]" style={{ width: `${percent}%` }} />
              </div>
            </div>
          )}

          {present.length > 0 && (
            <div className="space-y-2">
              {present.map(stem => {
                const url = apiUrl(`/v1/library/songs/${encodeURIComponent(song.id)}/stems/${stem}`);
                return (
                  <div key={stem} className="flex items-center gap-3 rounded-lg border border-zinc-200 px-3 py-2 dark:border-white/10">
                    <span className="w-20 shrink-0 text-xs font-semibold uppercase tracking-wide text-zinc-500">{t(`stem_${stem}` as never) || stem}</span>
                    <audio controls preload="none" src={url} className="h-8 min-w-0 flex-1" />
                    <a href={url} download={`${song.title} - ${stem}.wav`} className="shrink-0 text-zinc-400 hover:text-pink-500" title={t('download')}>
                      <Download size={15} />
                    </a>
                  </div>
                );
              })}
            </div>
          )}

          {(run?.error || error) && (
            <p role="alert" className="flex items-center gap-2 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-300">
              <AlertTriangle size={14} /> {run?.error || error}
            </p>
          )}
        </div>

        <div className="flex justify-end gap-2 border-t border-zinc-200 px-5 py-4 dark:border-white/10">
          <button type="button" onClick={onClose} className="rounded-lg px-4 py-2 text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200">
            {t('close')}
          </button>
          <button
            type="button"
            onClick={() => void start()}
            disabled={!status?.ready || running || busy}
            className="inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:cursor-not-allowed disabled:opacity-50"
          >
            {running ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />}
            {present.length > 0 ? t('stemsAgain') : t('stemsStart')}
          </button>
        </div>
      </div>
    </div>
  );
};
