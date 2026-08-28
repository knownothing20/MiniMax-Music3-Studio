import React from 'react';
import { CheckCircle2, Cloud, FlaskConical, Loader2, Route, Settings2, Sparkles } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import type { OmniBridgeIntegrationStatus } from '../services/omnibridgeMusic';

interface CloudFirstRunProps {
  checking: boolean;
  ready: boolean;
  status: OmniBridgeIntegrationStatus | null;
  onStartCreating: () => void;
  onOpenApiCase: () => void;
  onOpenLocalModels: () => void;
}

const EvidenceCard: React.FC<{
  active: boolean;
  checking: boolean;
  icon: React.ReactNode;
  label: string;
  detail?: string | null;
}> = ({ active, checking, icon, label, detail }) => (
  <div className={`rounded-2xl border p-4 transition-colors ${active
    ? 'border-emerald-400/35 bg-emerald-500/10'
    : 'border-zinc-200 bg-white/70 dark:border-white/10 dark:bg-white/5'}`}>
    <div className="flex items-center gap-3">
      <span className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-xl ${active
        ? 'bg-emerald-500 text-white'
        : 'bg-zinc-100 text-zinc-400 dark:bg-white/10 dark:text-zinc-500'}`}>
        {checking ? <Loader2 size={18} className="animate-spin" /> : icon}
      </span>
      <div className="min-w-0">
        <p className="text-sm font-semibold text-zinc-900 dark:text-white">{label}</p>
        {detail && <p className="mt-0.5 truncate font-mono text-[11px] text-zinc-500 dark:text-zinc-400">{detail}</p>}
      </div>
      {active && <CheckCircle2 size={18} className="ml-auto shrink-0 text-emerald-500" />}
    </div>
  </div>
);

export const CloudFirstRun: React.FC<CloudFirstRunProps> = ({
  checking,
  ready,
  status,
  onStartCreating,
  onOpenApiCase,
  onOpenLocalModels,
}) => {
  const { t } = useI18n();
  const gatewayReady = status?.configured === true && status.contractVerified === true;
  const routeReady = status?.routeResolutionVerified === true;
  const providerReady = status?.providerResolutionVerified === true;

  return (
    <div className="relative flex h-full min-h-0 flex-1 items-center justify-center overflow-y-auto bg-zinc-50 px-5 py-10 dark:bg-suno">
      <div className="pointer-events-none absolute inset-0 overflow-hidden">
        <div className="absolute left-[12%] top-[8%] h-72 w-72 rounded-full bg-pink-500/10 blur-3xl" />
        <div className="absolute bottom-[4%] right-[8%] h-80 w-80 rounded-full bg-purple-500/10 blur-3xl" />
      </div>

      <section className="relative w-full max-w-4xl overflow-hidden rounded-[28px] border border-zinc-200/80 bg-white/90 p-6 shadow-2xl shadow-pink-500/5 backdrop-blur-xl dark:border-white/10 dark:bg-suno-card/90 md:p-9">
        <div className="flex flex-col gap-7 md:flex-row md:items-start md:justify-between">
          <div className="max-w-2xl">
            <div className="inline-flex items-center gap-2 rounded-full border border-pink-500/20 bg-pink-500/10 px-3 py-1.5 text-xs font-semibold text-pink-600 dark:text-pink-300">
              {checking ? <Loader2 size={14} className="animate-spin" /> : <Cloud size={14} />}
              {checking ? t('cloudChecking') : ready ? t('cloudReadyBadge') : t('omniNotConfigured')}
            </div>
            <h1 className="mt-5 text-3xl font-extrabold tracking-tight text-zinc-950 dark:text-white md:text-4xl">
              {t('cloudFirstRunTitle')}
            </h1>
            <p className="mt-3 max-w-xl text-sm leading-6 text-zinc-600 dark:text-zinc-300">
              {t('cloudFirstRunSubtitle')}
            </p>
          </div>
          <div className="flex h-20 w-20 shrink-0 items-center justify-center rounded-3xl bg-gradient-to-br from-orange-500 via-pink-500 to-purple-600 text-white shadow-xl shadow-pink-500/20">
            <Sparkles size={34} />
          </div>
        </div>

        <div className="mt-8 grid gap-3 md:grid-cols-3">
          <EvidenceCard
            active={gatewayReady}
            checking={checking}
            icon={<Cloud size={19} />}
            label={t('cloudGatewayConnected')}
          />
          <EvidenceCard
            active={routeReady}
            checking={checking}
            icon={<Route size={19} />}
            label={t('cloudRouteReady')}
            detail={status?.musicRoute}
          />
          <EvidenceCard
            active={providerReady}
            checking={checking}
            icon={<Sparkles size={19} />}
            label={t('cloudProviderReady')}
            detail={status?.operation}
          />
        </div>

        <div className="mt-6 flex items-center gap-2 rounded-2xl border border-emerald-500/20 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-700 dark:text-emerald-200">
          <CheckCircle2 size={18} className="shrink-0" />
          <span>{t('cloudNoLocalDownload')}</span>
        </div>

        <div className="mt-7 flex flex-col gap-3 sm:flex-row sm:flex-wrap">
          <button
            type="button"
            onClick={onStartCreating}
            disabled={!ready || checking}
            className="inline-flex min-h-12 flex-1 items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-orange-500 to-pink-600 px-5 py-3 text-sm font-bold text-white shadow-lg shadow-pink-500/15 transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-45"
          >
            {checking ? <Loader2 size={17} className="animate-spin" /> : <Sparkles size={17} />}
            {checking ? t('cloudChecking') : t('cloudStartCreating')}
          </button>
          <button
            type="button"
            onClick={onOpenApiCase}
            className="inline-flex min-h-12 items-center justify-center gap-2 rounded-xl border border-zinc-300 px-5 py-3 text-sm font-semibold text-zinc-700 transition hover:border-pink-400 hover:text-pink-600 dark:border-white/15 dark:text-zinc-200"
          >
            <FlaskConical size={17} />
            {t('cloudOpenApiCase')}
          </button>
          <button
            type="button"
            onClick={onOpenLocalModels}
            className="inline-flex min-h-12 items-center justify-center gap-2 rounded-xl px-4 py-3 text-sm font-medium text-zinc-500 transition hover:bg-zinc-100 hover:text-zinc-800 dark:hover:bg-white/5 dark:hover:text-zinc-200"
          >
            <Settings2 size={17} />
            {t('cloudLocalOptional')}
          </button>
        </div>
      </section>
    </div>
  );
};
