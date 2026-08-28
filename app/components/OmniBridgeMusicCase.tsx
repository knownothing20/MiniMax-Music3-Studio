import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AlertTriangle, Check, Circle, Clock3, FlaskConical, Loader2, Music2, RotateCcw, Search } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import type { Music3Job } from '../types';
import {
  OmniBridgeApiError,
  OmniBridgeCaseIntent,
  OmniBridgeIntegrationStatus,
  VerifiedAudioArtifact,
  clearOmniBridgeCaseIntent,
  createOmniBridgeCaseIntent,
  getOmniBridgeMusicJob,
  loadOmniBridgeCaseIntent,
  readOmniBridgeIntegrationStatus,
  submitOmniBridgeMusicJobOnce,
  updateOmniBridgeCaseIntent,
  verifyImportedAudioArtifact,
} from '../services/omnibridgeMusic';

const EXAMPLE = {
  title: 'API 示例：霓虹夜航',
  caption: 'Cinematic synth-pop with warm analog pads, restrained electronic drums, a clear female lead vocal, and a hopeful late-night city atmosphere.',
  lyrics: '[Verse]\n霓虹沿着车窗慢慢后退\n沉睡的城市把心事包围\n\n[Chorus]\n陪我穿过这片夜色\n在天亮以前找到答案\n让风记住此刻的歌\n我们向着远方不再回看',
};
const POLL_BASE_DELAY_MS = 1800;
const POLL_MAX_DELAY_MS = 30_000;

type StepState = 'pending' | 'running' | 'pass' | 'warn' | 'fail';

interface TimelineStepProps {
  label: string;
  detail: string;
  state: StepState;
}

const TimelineStep: React.FC<TimelineStepProps> = ({ label, detail, state }) => {
  const icon = state === 'running'
    ? <Loader2 size={15} className="animate-spin" />
    : state === 'pass'
      ? <Check size={15} />
      : state === 'warn' || state === 'fail'
        ? <AlertTriangle size={15} />
        : <Circle size={15} />;
  const tone = state === 'pass'
    ? 'border-emerald-500/25 bg-emerald-500/5 text-emerald-700 dark:text-emerald-300'
    : state === 'warn'
      ? 'border-amber-500/25 bg-amber-500/5 text-amber-800 dark:text-amber-200'
      : state === 'fail'
        ? 'border-rose-500/25 bg-rose-500/5 text-rose-700 dark:text-rose-200'
        : state === 'running'
          ? 'border-pink-500/25 bg-pink-500/5 text-pink-700 dark:text-pink-200'
          : 'border-zinc-200 bg-zinc-50 text-zinc-500 dark:border-white/5 dark:bg-black/10 dark:text-zinc-400';
  return (
    <li className={`rounded-xl border px-3 py-2.5 ${tone}`}>
      <div className="flex items-center gap-2 text-xs font-semibold">{icon}<span>{label}</span></div>
      <p className="mt-1 break-words pl-[23px] text-[11px] leading-4 opacity-80">{detail}</p>
    </li>
  );
};

function isUnknown(job: Music3Job): boolean {
  const phase = job.phase?.toLowerCase() || '';
  return job.status === 'unknown' || phase === 'unknown' || phase === 'submission_unknown';
}

function completedSong(job: Music3Job | null) {
  return job?.songs?.[0] ?? job?.song;
}

export const OmniBridgeMusicCase: React.FC = () => {
  const { t } = useI18n();
  const [title, setTitle] = useState(EXAMPLE.title);
  const [caption, setCaption] = useState(EXAMPLE.caption);
  const [lyrics, setLyrics] = useState(EXAMPLE.lyrics);
  const [confirmed, setConfirmed] = useState(false);
  const [integration, setIntegration] = useState<OmniBridgeIntegrationStatus | null>(null);
  const [integrationBusy, setIntegrationBusy] = useState(true);
  const [intent, setIntent] = useState<OmniBridgeCaseIntent | null>(null);
  const intentRef = useRef<OmniBridgeCaseIntent | null>(null);
  const [job, setJob] = useState<Music3Job | null>(null);
  const [pollCount, setPollCount] = useState(0);
  const [busy, setBusy] = useState(false);
  const [polling, setPolling] = useState(false);
  const [artifact, setArtifact] = useState<VerifiedAudioArtifact | null>(null);
  const [artifactBusy, setArtifactBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [audioObjectUrl, setAudioObjectUrl] = useState<string | null>(null);
  const audioObjectUrlRef = useRef<string | null>(null);
  const timerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const pollBackoffRef = useRef(0);
  const queryRef = useRef<(automatic?: boolean) => Promise<void>>(async () => undefined);

  const replaceObjectUrl = useCallback((next: string | null) => {
    if (audioObjectUrlRef.current) URL.revokeObjectURL(audioObjectUrlRef.current);
    audioObjectUrlRef.current = next;
    setAudioObjectUrl(next);
  }, []);

  const saveIntent = useCallback((next: OmniBridgeCaseIntent) => {
    intentRef.current = next;
    setIntent(next);
    return next;
  }, []);

  const patchIntent = useCallback((patch: Parameters<typeof updateOmniBridgeCaseIntent>[1]) => {
    const current = intentRef.current;
    if (!current) throw new OmniBridgeApiError('The persisted API case intent is unavailable.');
    return saveIntent(updateOmniBridgeCaseIntent(current, patch));
  }, [saveIntent]);

  const stopTimer = useCallback(() => {
    if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    timerRef.current = null;
    setPolling(false);
  }, []);

  const scheduleGetOnlyPoll = useCallback(() => {
    stopTimer();
    setPolling(true);
    const delay = Math.min(POLL_BASE_DELAY_MS * (2 ** pollBackoffRef.current), POLL_MAX_DELAY_MS);
    timerRef.current = window.setTimeout(() => void queryRef.current(true), delay);
  }, [stopTimer]);

  const verifyCompletedJob = useCallback(async (completed: Music3Job) => {
    setArtifactBusy(true);
    setArtifact(null);
    replaceObjectUrl(null);
    try {
      const verified = await verifyImportedAudioArtifact(completed);
      setArtifact(verified);
      replaceObjectUrl(URL.createObjectURL(verified.blob));
      patchIntent({ submitOutcome: 'completed' });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : t('caseArtifactFailed'));
    } finally {
      setArtifactBusy(false);
    }
  }, [patchIntent, replaceObjectUrl, t]);

  const applyJob = useCallback(async (next: Music3Job) => {
    setJob(next);
    setError(null);
    if (next.status === 'completed') {
      stopTimer();
      await verifyCompletedJob(next);
      return;
    }
    if (isUnknown(next)) {
      stopTimer();
      patchIntent({ submitOutcome: 'unknown' });
      return;
    }
    if (next.status === 'failed' || next.status === 'cancelled') {
      stopTimer();
      patchIntent({ submitOutcome: 'rejected' });
      return;
    }
    patchIntent({ submitOutcome: 'accepted' });
    scheduleGetOnlyPoll();
  }, [patchIntent, scheduleGetOnlyPoll, stopTimer, verifyCompletedJob]);

  const queryJob = useCallback(async (automatic = false) => {
    const current = intentRef.current;
    if (!current?.postAttempted) return;
    if (!automatic) {
      stopTimer();
      pollBackoffRef.current = 0;
    } else {
      // Advance after the elapsed timer so the first scheduled GET remains
      // 1.8s, then successful or failed accepted queries back off to 30s.
      pollBackoffRef.current += 1;
    }
    setPolling(true);
    setPollCount(value => value + 1);
    try {
      const next = await getOmniBridgeMusicJob(current.jobId);
      pollBackoffRef.current = 0;
      await applyJob(next);
    } catch (reason) {
      stopTimer();
      const message = reason instanceof Error ? reason.message : t('caseQueryFailed');
      setError(message);
      // A status transport failure never authorizes another POST. Accepted jobs
      // may be queried again manually; an unknown submit stays explicitly unknown.
      if (current.submitOutcome === 'attempted') {
        patchIntent({ submitOutcome: 'unknown' });
      } else if (current.submitOutcome === 'accepted') {
        scheduleGetOnlyPoll();
      }
    }
  }, [applyJob, patchIntent, scheduleGetOnlyPoll, stopTimer, t]);
  queryRef.current = queryJob;

  const refreshIntegration = useCallback(async () => {
    setIntegrationBusy(true);
    try {
      const next = await readOmniBridgeIntegrationStatus();
      setIntegration(next);
      return next;
    } catch (reason) {
      setIntegration(null);
      setError(reason instanceof Error ? reason.message : t('omniStatusUnavailable'));
      return null;
    } finally {
      setIntegrationBusy(false);
    }
  }, [t]);

  useEffect(() => {
    void refreshIntegration();
    const restored = loadOmniBridgeCaseIntent();
    if (restored) {
      saveIntent(restored);
      setTitle(restored.title);
      setCaption(restored.caption);
      setLyrics(restored.lyrics);
      if (restored.postAttempted) void queryRef.current(false);
    }
    return () => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current);
      if (audioObjectUrlRef.current) URL.revokeObjectURL(audioObjectUrlRef.current);
    };
  }, [refreshIntegration, saveIntent]);

  const runOnce = useCallback(async () => {
    setError(null);
    setArtifact(null);
    replaceObjectUrl(null);
    const clean = { title: title.trim(), caption: caption.trim(), lyrics: lyrics.replace(/\r\n?/g, '\n').trim() };
    if (!clean.caption || !clean.lyrics) {
      setError(t('caseLyricsRequired'));
      return;
    }
    if (!confirmed) {
      setError(t('caseConfirmationRequired'));
      return;
    }
    setBusy(true);
    try {
      const status = await refreshIntegration();
      if (!status?.configured || status.executionTarget !== 'omnibridge') {
        throw new OmniBridgeApiError(t('caseIntegrationNotReady'));
      }
      if (!status.contractVerified) throw new OmniBridgeApiError(t('caseContractNotVerified'));
      let current = intentRef.current;
      if (current?.postAttempted) throw new OmniBridgeApiError(t('caseNoReplay'));
      if (current) {
        if (current.title !== clean.title || current.caption !== clean.caption || current.lyrics !== clean.lyrics) {
          throw new OmniBridgeApiError(t('caseIntentLocked'));
        }
      } else {
        current = saveIntent(createOmniBridgeCaseIntent(clean));
      }
      // This durable read-back happens before fetch. Once true, no UI path can
      // call POST for this intent again, even after a reload or response loss.
      current = patchIntent({ postAttempted: true, submitOutcome: 'attempted' });
      setConfirmed(false);
      let submitted: Music3Job;
      try {
        submitted = await submitOmniBridgeMusicJobOnce(clean, current.clientRequestId);
      } catch (reason) {
        const knownRejection = reason instanceof OmniBridgeApiError && reason.responseKnown && reason.status !== null && reason.status >= 400 && reason.status < 500;
        patchIntent({ submitOutcome: knownRejection ? 'rejected' : 'unknown' });
        throw reason;
      }
      if (submitted.id !== current.jobId) {
        patchIntent({ submitOutcome: 'unknown' });
        throw new OmniBridgeApiError('The submit response returned an unexpected recovery handle; automatic replay is disabled.');
      }
      await applyJob(submitted);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : t('caseSubmitFailed'));
    } finally {
      setBusy(false);
    }
  }, [applyJob, caption, confirmed, lyrics, patchIntent, refreshIntegration, replaceObjectUrl, saveIntent, t, title]);

  const resetKnownCase = useCallback(() => {
    const current = intentRef.current;
    const terminal = current?.postAttempted === false || current?.submitOutcome === 'completed' || current?.submitOutcome === 'rejected';
    if (current && !terminal) {
      setError(t('caseNoReplay'));
      return;
    }
    stopTimer();
    clearOmniBridgeCaseIntent();
    intentRef.current = null;
    setIntent(null);
    setJob(null);
    setPollCount(0);
    setArtifact(null);
    setError(null);
    setConfirmed(false);
    replaceObjectUrl(null);
    setTitle(EXAMPLE.title);
    setCaption(EXAMPLE.caption);
    setLyrics(EXAMPLE.lyrics);
  }, [replaceObjectUrl, stopTimer, t]);

  const submitState: StepState = !intent
    ? 'pending'
    : !intent.postAttempted
      ? 'pending'
      : intent.submitOutcome === 'unknown'
        ? 'warn'
        : intent.submitOutcome === 'rejected'
          ? 'fail'
          : intent.submitOutcome === 'attempted'
            ? 'running'
            : 'pass';
  const routeVerified = integration?.routeResolutionVerified === true && integration?.providerResolutionVerified === true;
  const configState: StepState = integrationBusy ? 'running' : integration?.contractVerified && routeVerified ? 'pass' : integration ? 'warn' : 'fail';
  const acceptedState: StepState = isUnknown(job || ({ status: intent?.submitOutcome === 'unknown' ? 'unknown' : 'queued', phase: '' } as Music3Job))
    ? 'warn'
    : job ? (job.status === 'failed' || job.status === 'cancelled' ? 'fail' : 'pass') : 'pending';
  const pollState: StepState = polling ? 'running' : job?.status === 'completed' ? 'pass' : pollCount > 0 && error ? 'warn' : pollCount > 0 ? 'pass' : 'pending';
  const completed = completedSong(job);
  const canSubmit = !busy && confirmed && Boolean(caption.trim()) && Boolean(lyrics.trim()) && !intent?.postAttempted;
  const canReset = !intent || !intent.postAttempted || intent.submitOutcome === 'completed' || intent.submitOutcome === 'rejected';

  const steps = useMemo<TimelineStepProps[]>(() => [
    {
      label: t('caseStepConfig'),
      detail: integration
        ? `${integration.contractStatus}; route=${integration.routeReadiness}; provider=${integration.providerResolutionVerified ? 'verified' : 'unverified'}`
        : t('omniStatusUnavailable'),
      state: configState,
    },
    { label: t('caseStepIntent'), detail: intent ? `${intent.clientRequestId} · ${intent.submitOutcome}` : t('casePending'), state: intent ? 'pass' : 'pending' },
    { label: t('caseStepSubmit'), detail: intent?.postAttempted ? t('caseSinglePostRecorded') : t('casePending'), state: submitState },
    { label: t('caseStepAccepted'), detail: job?.message || (intent?.submitOutcome === 'unknown' ? t('caseNoReplay') : t('casePending')), state: acceptedState },
    { label: t('caseStepPoll'), detail: `${t('casePollCount')}: ${pollCount} · ${job?.status || t('casePending')}`, state: pollState },
    { label: t('caseStepArtifact'), detail: artifact ? `${artifact.contentType} · ${artifact.bytes} B · ${artifact.sha256}` : artifactBusy ? t('caseVerifyingArtifact') : t('casePending'), state: artifact ? 'pass' : artifactBusy ? 'running' : error && job?.status === 'completed' ? 'fail' : 'pending' },
    { label: t('caseStepLibrary'), detail: completed?.song?.metadata?.omnibridge_job_id ? `${completed.id} · ${completed.song.metadata.omnibridge_job_id}` : t('casePending'), state: completed?.song?.metadata?.omnibridge_job_id ? 'pass' : 'pending' },
    { label: t('caseStepPlayer'), detail: audioObjectUrl ? t('casePlayerReady') : t('casePending'), state: audioObjectUrl ? 'pass' : 'pending' },
  ], [acceptedState, artifact, artifactBusy, audioObjectUrl, completed, configState, error, integration, intent, job, pollCount, pollState, submitState, t]);

  return (
    <div className="h-full min-h-0 flex-1 overflow-y-auto bg-zinc-50 px-4 py-6 dark:bg-suno md:px-8">
      <div className="mx-auto max-w-6xl space-y-5">
        <header className="rounded-2xl border border-pink-500/20 bg-gradient-to-br from-pink-500/10 via-white to-orange-500/5 p-5 dark:via-suno-card">
          <p className="flex items-center gap-2 text-xs font-bold uppercase tracking-[0.18em] text-pink-600 dark:text-pink-300"><FlaskConical size={15} /> API Case</p>
          <h1 className="mt-2 text-2xl font-bold text-zinc-950 dark:text-white">{t('apiCaseTitle')}</h1>
          <p className="mt-2 max-w-3xl text-sm leading-6 text-zinc-600 dark:text-zinc-300">{t('apiCaseIntro')}</p>
        </header>

        <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(320px,0.8fr)]">
          <section className="space-y-4 rounded-2xl border border-zinc-200 bg-white p-5 shadow-sm dark:border-white/5 dark:bg-suno-card">
            <div className="rounded-xl border border-amber-500/30 bg-amber-500/10 p-3 text-xs leading-5 text-amber-900 dark:text-amber-100">
              <div className="flex gap-2"><AlertTriangle size={15} className="mt-0.5 shrink-0" /><span><b>{t('caseExampleBadge')}</b> {t('apiCasePaidWarning')}</span></div>
            </div>
            <label className="block text-xs font-semibold text-zinc-700 dark:text-zinc-200">
              {t('caseSongTitle')}
              <input value={title} onChange={event => setTitle(event.target.value)} disabled={Boolean(intent)} className="mt-1.5 w-full rounded-lg border border-zinc-300 bg-white px-3 py-2 text-sm outline-none focus:border-pink-500 disabled:opacity-60 dark:border-white/10 dark:bg-black/20" />
            </label>
            <label className="block text-xs font-semibold text-zinc-700 dark:text-zinc-200">
              {t('caseCaption')}
              <textarea value={caption} onChange={event => setCaption(event.target.value)} disabled={Boolean(intent)} rows={4} className="mt-1.5 w-full resize-y rounded-lg border border-zinc-300 bg-white px-3 py-2 text-sm leading-5 outline-none focus:border-pink-500 disabled:opacity-60 dark:border-white/10 dark:bg-black/20" />
            </label>
            <label className="block text-xs font-semibold text-zinc-700 dark:text-zinc-200">
              {t('caseLyrics')} <span className="text-rose-500">*</span>
              <textarea value={lyrics} onChange={event => setLyrics(event.target.value)} disabled={Boolean(intent)} rows={9} className="mt-1.5 w-full resize-y rounded-lg border border-zinc-300 bg-white px-3 py-2 text-sm leading-5 outline-none focus:border-pink-500 disabled:opacity-60 dark:border-white/10 dark:bg-black/20" />
            </label>

            {!intent?.postAttempted && (
              <label className="flex cursor-pointer items-start gap-2 rounded-xl border border-zinc-200 bg-zinc-50 p-3 text-xs leading-5 text-zinc-700 dark:border-white/10 dark:bg-black/15 dark:text-zinc-200">
                <input type="checkbox" checked={confirmed} onChange={event => setConfirmed(event.target.checked)} className="mt-1" />
                <span>{t('caseBillingConfirmation')}</span>
              </label>
            )}

            {error && <p role="alert" className="rounded-xl bg-rose-500/10 px-3 py-2 text-xs leading-5 text-rose-700 dark:text-rose-200">{error}</p>}

            <div className="flex flex-wrap gap-2">
              {!intent?.postAttempted && (
                <button type="button" onClick={() => void runOnce()} disabled={!canSubmit} className="inline-flex items-center gap-2 rounded-xl bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2.5 text-xs font-bold text-white disabled:cursor-not-allowed disabled:opacity-50">
                  {busy ? <Loader2 size={15} className="animate-spin" /> : <FlaskConical size={15} />} {t('caseRunOnce')}
                </button>
              )}
              {intent?.postAttempted && intent.submitOutcome !== 'completed' && intent.submitOutcome !== 'rejected' && (
                <button type="button" onClick={() => void queryJob(false)} disabled={polling} className="inline-flex items-center gap-2 rounded-xl border border-zinc-300 px-4 py-2.5 text-xs font-semibold text-zinc-700 disabled:opacity-50 dark:border-white/15 dark:text-zinc-200">
                  {polling ? <Loader2 size={15} className="animate-spin" /> : <Search size={15} />} {t('caseContinueGet')}
                </button>
              )}
              <button type="button" onClick={resetKnownCase} disabled={!canReset} className="inline-flex items-center gap-2 rounded-xl border border-zinc-300 px-4 py-2.5 text-xs font-semibold text-zinc-600 disabled:cursor-not-allowed disabled:opacity-40 dark:border-white/15 dark:text-zinc-300">
                <RotateCcw size={14} /> {t('caseNew')}
              </button>
            </div>

            {intent && (
              <div className="rounded-xl bg-zinc-950 p-3 font-mono text-[11px] leading-5 text-zinc-300">
                <div>client_request_id: {intent.clientRequestId}</div>
                <div>job_id: {intent.jobId}</div>
                <div>submit: {intent.submitOutcome}</div>
              </div>
            )}
          </section>

          <section className="space-y-4">
            <div className="rounded-2xl border border-zinc-200 bg-white p-5 shadow-sm dark:border-white/5 dark:bg-suno-card">
              <h2 className="flex items-center gap-2 text-sm font-bold text-zinc-900 dark:text-white"><Clock3 size={16} className="text-pink-500" /> {t('caseTimeline')}</h2>
              <p className="mt-1 text-[11px] leading-4 text-zinc-500">{t('caseNoReplay')}</p>
              <ol className="mt-4 space-y-2">{steps.map(step => <TimelineStep key={step.label} {...step} />)}</ol>
            </div>

            {audioObjectUrl && artifact && (
              <div className="rounded-2xl border border-emerald-500/25 bg-emerald-500/5 p-5">
                <h2 className="flex items-center gap-2 text-sm font-bold text-emerald-800 dark:text-emerald-200"><Music2 size={16} /> {t('casePlayerReady')}</h2>
                <audio className="mt-3 w-full" controls preload="metadata" src={audioObjectUrl} />
                <dl className="mt-3 space-y-1 break-all font-mono text-[10px] text-zinc-600 dark:text-zinc-300">
                  <div>sha256: {artifact.sha256}</div>
                  <div>bytes: {artifact.bytes}</div>
                  <div>mime: {artifact.contentType}</div>
                </dl>
              </div>
            )}
          </section>
        </div>
      </div>
    </div>
  );
};
