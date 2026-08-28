import React, { useCallback, useEffect, useState } from 'react';
import { AlertTriangle, CheckCircle2, FlaskConical, Loader2, RefreshCw, Route, ShieldCheck } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { OmniBridgeIntegrationStatus, readOmniBridgeIntegrationStatus } from '../services/omnibridgeMusic';

interface OmniBridgeSettingsProps {
  onOpenCase: () => void;
}

const ValueRow: React.FC<{ label: string; value: string; mono?: boolean }> = ({ label, value, mono }) => (
  <div className="grid gap-1 border-b border-zinc-200/70 py-2.5 last:border-0 dark:border-white/5 sm:grid-cols-[170px_1fr] sm:gap-4">
    <dt className="text-xs font-medium text-zinc-500 dark:text-zinc-400">{label}</dt>
    <dd className={`break-words text-xs text-zinc-800 dark:text-zinc-100 ${mono ? 'font-mono' : ''}`}>{value}</dd>
  </div>
);

export const OmniBridgeSettings: React.FC<OmniBridgeSettingsProps> = ({ onOpenCase }) => {
  const { t } = useI18n();
  const [status, setStatus] = useState<OmniBridgeIntegrationStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await readOmniBridgeIntegrationStatus());
    } catch (reason) {
      setStatus(null);
      setError(reason instanceof Error ? reason.message : t('omniStatusUnavailable'));
    } finally {
      setBusy(false);
    }
  }, [t]);

  useEffect(() => { void refresh(); }, [refresh]);

  const ready = status?.configured === true && status.executionTarget === 'omnibridge' && status.contractVerified === true;
  const evidence = status?.realGenerationVerified === true;
  const routeReady = status?.routeResolutionVerified === true && status?.providerResolutionVerified === true;

  return (
    <div className="max-w-2xl space-y-5">
      <section className="rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-black/15">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <h4 className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
              <ShieldCheck size={17} className="text-pink-500" /> OmniBridge Music
            </h4>
            <p className="mt-1 max-w-xl text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('omniSettingsIntro')}</p>
          </div>
          <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[10px] font-semibold ${ready ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-700 dark:text-amber-300'}`}>
            {ready ? <CheckCircle2 size={12} /> : <AlertTriangle size={12} />}
            {ready ? t('omniConfigured') : t('omniNotConfigured')}
          </span>
        </div>

        {status && (
          <dl className="mt-4 rounded-lg bg-zinc-50 px-3 dark:bg-black/20">
            <ValueRow label={t('omniExecutionTarget')} value={status.executionTarget} mono />
            <ValueRow label={t('omniRoute')} value={status.musicRoute || t('omniUnavailable')} mono />
            <ValueRow label={t('omniOperation')} value={status.operation || t('omniUnavailable')} mono />
            <ValueRow label={t('omniContractClient')} value={status.contractClient} mono />
            <ValueRow label={t('omniDiagnosticStatus')} value={status.diagnosticStatus} mono />
            <ValueRow label={t('omniContractStatus')} value={status.contractVerified ? `${status.contractStatus} · ${t('omniVerified')}` : `${status.contractStatus} · ${t('omniUnverified')}`} mono />
            <ValueRow label={t('omniRouteReadiness')} value={routeReady ? `${status.routeReadiness} · ${t('omniVerified')}` : `${status.routeReadiness} · ${t('omniUnverified')}`} mono />
            <ValueRow label={t('omniProviderResolution')} value={status.providerResolutionVerified ? t('omniVerified') : t('omniUnverified')} />
            <ValueRow label={t('omniRealGeneration')} value={evidence ? t('omniVerified') : t('omniUnverified')} />
          </dl>
        )}

        {status?.error && (
          <p role="alert" className="mt-3 flex gap-2 rounded-lg bg-amber-500/10 px-3 py-2 text-xs leading-5 text-amber-800 dark:text-amber-200">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" /> {status.error}
          </p>
        )}
        {error && (
          <p role="alert" className="mt-3 flex gap-2 rounded-lg bg-rose-500/10 px-3 py-2 text-xs leading-5 text-rose-700 dark:text-rose-200">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" /> {error}
          </p>
        )}

        <div className="mt-4 flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => void refresh()}
            disabled={busy}
            className="inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-2 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 disabled:opacity-50 dark:border-white/15 dark:text-zinc-200"
          >
            {busy ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />} {t('omniRefresh')}
          </button>
          <button
            type="button"
            onClick={onOpenCase}
            className="inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-3 py-2 text-xs font-bold text-white"
          >
            <FlaskConical size={14} /> {t('omniOpenCase')}
          </button>
        </div>
      </section>

      <section className="rounded-xl border border-indigo-500/20 bg-indigo-500/5 p-4 text-xs leading-5 text-zinc-600 dark:text-zinc-300">
        <p className="flex items-start gap-2">
          <Route size={15} className="mt-0.5 shrink-0 text-indigo-500" />
          <span>{t('omniCentralManaged')}</span>
        </p>
      </section>
    </div>
  );
};
