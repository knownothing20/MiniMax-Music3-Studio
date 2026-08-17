import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { ChevronDown, CircleAlert, Loader2, Music2, Settings2, Sparkles, Square, Terminal, Wand2 } from 'lucide-react';
import type { Music3Request, Song } from '../types';
import { useI18n } from '../context/I18nContext';

/**
 * The Music3 authoring panel.
 *
 * Every control here maps to a real field of the request the pinned
 * minimaxmusic.cpp accepts — nothing is decorative. The engine's own defaults
 * (60 s, 30 steps, LM CFG 1.5, top-k 50, DiT CFG 1.7, peak clip 10, MP3 128k)
 * are the quality baseline, and "Reset" returns to exactly those values.
 *
 * The caption/lyrics assistant is optional and cloud-only: it is enabled only
 * when a catalog-verified OpenRouter text model is configured, because there is
 * no local language model in this build.
 */

interface CreatePanelProps {
  onGenerate: (request: Music3Request & { _tempId?: string }) => void;
  isGenerating: boolean;
  activeJobCount?: number;
  initialData?: { song: Song; timestamp: number } | null;
}

type SetupStatus = {
  ready?: boolean;
  engine_ready?: boolean;
  selected_profile_id?: string | null;
  selected_component_ids?: string[] | null;
  recommended_profile_id?: string;
  hardware?: { gpuName?: string; totalVramGb?: number; recommended?: string; reason?: string };
};

/** Upstream `request_init` defaults — the quality baseline, not a guess. */
const QUALITY = {
  duration: 60,
  steps: 30,
  lmCfg: 1.5,
  topK: 50,
  ditCfg: 1.7,
  lmBatch: 1,
  synthBatch: 1,
  peakClip: 10,
  mp3Bitrate: 128,
} as const;

const PROFILE_LABEL: Record<string, string> = {
  native: 'Full Native (BF16/BF16/F32)',
  'quality-q8': 'Q8 Quality',
  balanced: 'Balanced',
  'recommended-light': 'Light (low VRAM / speed)',
};

const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-zinc-50 p-2.5 text-sm text-zinc-900 outline-none focus:border-pink-500 dark:border-white/10 dark:bg-black/20 dark:text-white';

const TEMPLATES = [
  {
    label: 'Pop',
    caption: 'Modern emotional pop song, polished production, memorable chorus, warm female lead vocal',
    lyrics: '[Verse]\nI was lost in the city lights\nLooking for a way back home\n\n[Chorus]\nHold on, we are not alone\nTonight our hearts will find the way',
  },
  {
    label: 'Electronic',
    caption: 'Cinematic melodic electronic track, driving four-on-the-floor beat, wide synths, nocturnal atmosphere, male vocal',
    lyrics: '[Verse]\nNeon on the empty street\nThe night is moving to the beat\n\n[Chorus]\nWe run into the afterglow\nWhere only dreamers ever go',
  },
  {
    label: 'Rock',
    caption: 'Energetic alternative rock, live drums, distorted guitars, anthemic male vocal, dynamic chorus',
    lyrics: '[Verse]\nDust on my shoes, fire in my veins\nI learned to dance through all the rain\n\n[Chorus]\nTurn it up, let the whole world know\nWe are alive and we will not let go',
  },
];

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

export const CreatePanel: React.FC<CreatePanelProps> = ({ onGenerate, isGenerating, activeJobCount = 0, initialData }) => {
  const { t } = useI18n();
  const [mode, setMode] = useState<'simple' | 'manual'>('simple');
  const [caption, setCaption] = useState('');
  const [lyrics, setLyrics] = useState('');
  const [title, setTitle] = useState('');
  const [idea, setIdea] = useState('');

  const [duration, setDuration] = useState<number>(QUALITY.duration);
  const [steps, setSteps] = useState<number>(QUALITY.steps);
  const [lmCfg, setLmCfg] = useState<number>(QUALITY.lmCfg);
  const [topK, setTopK] = useState<number>(QUALITY.topK);
  const [ditCfg, setDitCfg] = useState<number>(QUALITY.ditCfg);
  const [lmBatch, setLmBatch] = useState<number>(QUALITY.lmBatch);
  const [synthBatch, setSynthBatch] = useState<number>(QUALITY.synthBatch);
  const [peakClip, setPeakClip] = useState<number>(QUALITY.peakClip);
  const [mp3Bitrate, setMp3Bitrate] = useState<number>(QUALITY.mp3Bitrate);
  const [format, setFormat] = useState<Music3Request['output_format']>('mp3');
  const [seed, setSeed] = useState('');
  const [lmSeed, setLmSeed] = useState('');

  const [showAdvanced, setShowAdvanced] = useState(false);
  const [showLogs, setShowLogs] = useState(false);
  const [logs, setLogs] = useState<string[]>([]);
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [assistantModel, setAssistantModel] = useState<string | null>(null);
  const [assisting, setAssisting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const ready = setup?.ready === true && setup?.engine_ready === true;
  const profileLabel = useMemo(() => {
    if (setup?.selected_component_ids?.length) return 'Custom component set';
    const id = setup?.selected_profile_id;
    if (!id) return 'no profile selected';
    return PROFILE_LABEL[id] ?? id;
  }, [setup]);

  const refreshSetup = useCallback(async () => {
    const response = await fetch('/setup/status');
    if (!response.ok) throw new Error(`Local engine status is unavailable (${response.status})`);
    setSetup(await response.json());
  }, []);

  useEffect(() => {
    void refreshSetup().catch((reason: Error) => setError(reason.message));
  }, [refreshSetup]);

  // The assistant is only offered when a catalog-verified text model is the
  // configured provider for prompt enhancement.
  useEffect(() => {
    void fetch('/v1/configuration')
      .then(response => response.json())
      .then((configuration: { selections?: Array<{ capability: string; mode: string; cloud_model: string | null }> }) => {
        const selection = configuration.selections?.find(item => item.capability === 'prompt_enhancement');
        setAssistantModel(selection?.mode === 'open_router' ? selection.cloud_model : null);
      })
      .catch(() => setAssistantModel(null));
  }, []);

  useEffect(() => {
    if (!initialData?.song) return;
    const song = initialData.song;
    setTitle(song.title || '');
    setCaption(song.style || '');
    setLyrics(song.lyrics || '');
    setMode('manual');
    const settings = song.generationParams as Partial<Record<string, number | string>> | undefined;
    if (settings) {
      if (typeof settings.duration === 'number') setDuration(settings.duration);
      if (typeof settings.steps === 'number') setSteps(settings.steps);
      if (typeof settings.lm_cfg === 'number') setLmCfg(settings.lm_cfg);
      if (typeof settings.lm_top_k === 'number') setTopK(settings.lm_top_k);
      if (typeof settings.dit_cfg === 'number') setDitCfg(settings.dit_cfg);
      if (typeof settings.peak_clip === 'number') setPeakClip(settings.peak_clip);
      if (typeof settings.mp3_bitrate === 'number') setMp3Bitrate(settings.mp3_bitrate);
      if (typeof settings.output_format === 'string') setFormat(settings.output_format as Music3Request['output_format']);
    }
  }, [initialData]);

  // Engine logs are the only fine-grained progress upstream exposes.
  useEffect(() => {
    if (!showLogs) return;
    const poll = () => void fetch('/v1/engine/logs')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error(String(response.status)))))
      .then((body: { lines?: string[] }) => setLogs((body.lines ?? []).slice(-120)))
      .catch(() => setLogs(['Engine logs are unavailable.']));
    poll();
    const timer = window.setInterval(poll, 2000);
    return () => window.clearInterval(timer);
  }, [showLogs]);

  const applyTemplate = (template: typeof TEMPLATES[number]) => {
    setCaption(template.caption);
    setLyrics(template.lyrics);
    setMode('manual');
  };

  const restoreQuality = () => {
    setDuration(QUALITY.duration);
    setSteps(QUALITY.steps);
    setLmCfg(QUALITY.lmCfg);
    setTopK(QUALITY.topK);
    setDitCfg(QUALITY.ditCfg);
    setLmBatch(QUALITY.lmBatch);
    setSynthBatch(QUALITY.synthBatch);
    setPeakClip(QUALITY.peakClip);
    setMp3Bitrate(QUALITY.mp3Bitrate);
    setFormat('mp3');
    setSeed('');
    setLmSeed('');
  };

  const runAssistant = async () => {
    if (!assistantModel || assisting) return;
    const brief = (idea || caption).trim();
    if (!brief) {
      setError('Describe the track first, then ask the assistant.');
      return;
    }
    setAssisting(true);
    setError(null);
    try {
      const response = await fetch('/v1/openrouter/completions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          model_id: assistantModel,
          prompt:
            'You write inputs for a text-to-music model. Answer with JSON only, no code fence, using the keys ' +
            '"title", "caption" and "lyrics". "caption" is one English sentence describing genre, instrumentation, ' +
            'mood and vocal. "lyrics" uses [Verse] / [Chorus] section tags on their own lines. Brief: ' + brief,
        }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || `Assistant request failed (${response.status})`);
      const content: unknown = body?.body?.choices?.[0]?.message?.content;
      if (typeof content !== 'string') throw new Error('The assistant returned no text.');
      const json = content.slice(content.indexOf('{'), content.lastIndexOf('}') + 1);
      const draft = JSON.parse(json) as { title?: string; caption?: string; lyrics?: string };
      if (draft.title) setTitle(draft.title);
      if (draft.caption) setCaption(draft.caption);
      if (draft.lyrics) setLyrics(draft.lyrics);
      setMode('manual');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'The assistant request failed.');
    } finally {
      setAssisting(false);
    }
  };

  const submit = () => {
    const cleanCaption = caption.trim();
    const cleanLyrics = lyrics.replace(/\r\n?/g, '\n').trim();
    if (!ready) {
      setError('Install and select a complete Music3 profile in the model manager first.');
      return;
    }
    if (!cleanCaption) {
      setError('Add a caption describing the track.');
      return;
    }
    if (!cleanLyrics) {
      setError('Music3 needs explicit lyrics. Write them, or apply a template.');
      return;
    }
    const parseSeed = (value: string, label: string) => {
      if (value.trim() === '') return undefined;
      const parsed = Number(value);
      if (!Number.isInteger(parsed) || parsed < 0) throw new Error(`${label} must be a non-negative integer.`);
      return parsed;
    };
    try {
      setError(null);
      onGenerate({
        caption: cleanCaption,
        lyrics: cleanLyrics,
        duration_seconds: duration,
        steps,
        seed: parseSeed(seed, 'Seed'),
        lm_seed: parseSeed(lmSeed, 'LM seed'),
        lm_cfg: lmCfg,
        lm_top_k: topK,
        lm_batch_size: lmBatch,
        synth_batch_size: synthBatch,
        dit_cfg: ditCfg,
        peak_clip: peakClip,
        output_format: format,
        mp3_bitrate: mp3Bitrate,
        title: title.trim() || undefined,
      });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Invalid generation settings.');
    }
  };

  const totalTracks = lmBatch * synthBatch;

  return (
    <section className="flex h-full min-h-0 w-full flex-col overflow-hidden bg-zinc-50 text-zinc-900 dark:bg-suno-panel dark:text-white">
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain custom-scrollbar">
        <div className="space-y-4 p-4 pb-6 pt-4">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h1 className="truncate text-base font-bold">{t('createMusic') || 'Create music'}</h1>
              <p className="mt-0.5 truncate text-[11px] text-zinc-500 dark:text-zinc-400">MiniMax Music 3 · local C++/CUDA inference</p>
            </div>
            <span className={`shrink-0 rounded-full px-2.5 py-1 text-[10px] font-semibold ${ready ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-700 dark:text-amber-300'}`}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${ready ? 'bg-emerald-500' : 'bg-amber-500'}`} />
              {ready ? t('engineReady') : t('profileRequired')}
            </span>
          </div>

          <div className="grid grid-cols-2 rounded-xl border border-zinc-200 bg-white p-1 dark:border-white/10 dark:bg-black/20">
            {(['simple', 'manual'] as const).map(value => (
              <button
                key={value}
                type="button"
                onClick={() => setMode(value)}
                className={`rounded-lg py-2 text-xs font-semibold transition-colors ${mode === value ? 'bg-zinc-900 text-white shadow-sm dark:bg-white dark:text-zinc-900' : 'text-zinc-500 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-white'}`}
              >
                {value === 'simple' ? (t('simpleMode') || 'Simple') : (t('customMode') || 'Manual')}
              </button>
            ))}
          </div>

          {!ready && (
            <div className="flex gap-2 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-xs leading-5 text-amber-800 dark:text-amber-200">
              <CircleAlert className="mt-0.5 shrink-0" size={15} />
              <div>
                <b>Local generation is not available yet.</b>
                <br />
                Choose and download a complete five-component Music3 profile in the model manager. Nothing downloads by itself.
              </div>
            </div>
          )}

          {mode === 'simple' ? (
            <div className="space-y-3 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
              <Field label={t('songDescription') || 'Track idea'}>
                <textarea
                  value={idea}
                  onChange={event => setIdea(event.target.value)}
                  placeholder="e.g. atmospheric synth-pop about a night drive, memorable chorus"
                  className={`${CONTROL} h-24 resize-none`}
                />
              </Field>
              <button
                type="button"
                onClick={() => void runAssistant()}
                disabled={!assistantModel || assisting}
                title={assistantModel ? undefined : 'Select an OpenRouter text model for the assistant in Settings'}
                className="inline-flex w-full items-center justify-center gap-2 rounded-lg border border-zinc-200 py-2.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-pink-400 hover:text-pink-600 disabled:cursor-not-allowed disabled:opacity-50 dark:border-white/10 dark:text-zinc-200"
              >
                {assisting ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} className="text-pink-500" />}
                {t('draftWithAssistant')}
              </button>
              <div className="flex items-center justify-between">
                <span className="text-xs font-bold uppercase tracking-wide text-zinc-500">{t('quickStart')}</span>
              </div>
              <div className="grid grid-cols-3 gap-2">
                {TEMPLATES.map(template => (
                  <button
                    key={template.label}
                    type="button"
                    onClick={() => applyTemplate(template)}
                    className="rounded-lg border border-zinc-200 px-2 py-2 text-[11px] font-medium hover:border-pink-400 hover:bg-pink-50 dark:border-white/10 dark:hover:bg-pink-500/10"
                  >
                    {template.label}
                  </button>
                ))}
              </div>
              <p className="text-[11px] leading-4 text-zinc-500 dark:text-zinc-400">
                Music3 sings explicit lyrics. A template fills an editable caption and lyric sheet without contacting any service.
              </p>
            </div>
          ) : null}

          <div className="space-y-3 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
            <Field label={t('styleOfMusic') || 'Caption / style'}>
              <textarea
                value={caption}
                onChange={event => setCaption(event.target.value)}
                placeholder="Genre, instruments, mood, vocal, arrangement"
                className={`${CONTROL} h-24 resize-none`}
              />
            </Field>
            <Field label={t('lyrics') || 'Lyrics'}>
              <textarea
                value={lyrics}
                onChange={event => setLyrics(event.target.value)}
                placeholder={'[Verse]\n...\n\n[Chorus]\n...'}
                className={`${CONTROL} h-44 resize-none font-mono text-xs`}
              />
            </Field>
            <Field label={`${t('title')} · ${t('libraryOnly')}`}>
              <input value={title} onChange={event => setTitle(event.target.value)} placeholder="Untitled" className={CONTROL} />
            </Field>
          </div>

          <div className="rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
            <div className="mb-3 flex items-center justify-between">
              <div className="flex items-center gap-2 text-xs font-bold uppercase tracking-wide text-zinc-500">
                <Music2 size={14} />
                {t('quality')}
              </div>
              <button type="button" onClick={restoreQuality} className="text-[11px] font-semibold text-pink-600 hover:text-pink-500">
                {t('resetToDefaults')}
              </button>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <NumberField label="Duration, s" value={duration} min={10} max={300} step={5} onChange={setDuration} />
              <NumberField label="DiT steps" value={steps} min={2} max={80} step={1} onChange={setSteps} />
            </div>
            <p className="mt-3 text-[11px] leading-4 text-zinc-500 dark:text-zinc-400">
              Engine defaults: 60 s · 30 steps · LM CFG 1.5 · top-k 50 · DiT CFG 1.7.
            </p>
          </div>

          <button
            type="button"
            onClick={() => setShowAdvanced(value => !value)}
            className="flex w-full items-center justify-between rounded-xl border border-zinc-200 bg-white px-4 py-3 text-sm font-semibold dark:border-white/5 dark:bg-suno-card"
          >
            <span className="flex items-center gap-2">
              <Settings2 size={16} />
              {t('advanced')}
            </span>
            <ChevronDown size={16} className={showAdvanced ? 'rotate-180 transition-transform' : 'transition-transform'} />
          </button>

          {showAdvanced && (
            <div className="space-y-4 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
              <div className="grid grid-cols-2 gap-3">
                <NumberField label="LM CFG" value={lmCfg} min={0.5} max={4} step={0.1} onChange={setLmCfg} />
                <NumberField label="LM top-k" value={topK} min={1} max={200} step={1} onChange={setTopK} />
                <NumberField label="DiT CFG" value={ditCfg} min={0.5} max={5} step={0.1} onChange={setDitCfg} />
                <NumberField label="Peak clip" value={peakClip} min={0} max={1000} step={1} onChange={setPeakClip} />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <NumberField label="Songs (LM batch)" value={lmBatch} min={1} max={4} step={1} onChange={setLmBatch} />
                <NumberField label="Variations per song" value={synthBatch} min={1} max={9} step={1} onChange={setSynthBatch} />
              </div>
              <p className="text-[11px] leading-4 text-zinc-500 dark:text-zinc-400">
                This request renders <b>{totalTracks}</b> {totalTracks === 1 ? 'track' : 'tracks'}: each song samples its own LM
                stream, each variation re-runs flow matching on the same condition track.
              </p>
              <div className="grid grid-cols-2 gap-3">
                <Field label="DiT seed (blank = random)">
                  <input inputMode="numeric" value={seed} onChange={event => setSeed(event.target.value)} placeholder="Random" className={CONTROL} />
                </Field>
                <Field label="LM seed (blank = random)">
                  <input inputMode="numeric" value={lmSeed} onChange={event => setLmSeed(event.target.value)} placeholder="Random" className={CONTROL} />
                </Field>
              </div>
              <div className="grid grid-cols-2 gap-3">
                <Field label="Output format">
                  <select value={format} onChange={event => setFormat(event.target.value as Music3Request['output_format'])} className={CONTROL}>
                    <option value="mp3">MP3</option>
                    <option value="wav16">WAV 16-bit</option>
                    <option value="wav24">WAV 24-bit</option>
                    <option value="wav32">WAV 32-bit float</option>
                  </select>
                </Field>
                <NumberField label="MP3 bitrate, kbps" value={mp3Bitrate} min={64} max={320} step={16} onChange={setMp3Bitrate} disabled={format !== 'mp3'} />
              </div>
              <p className="text-[11px] leading-4 text-zinc-500">
                Peak clip normalises to the (1 − clip/1e6) percentile; 0 disables clipping and WAV 32-bit float skips it entirely.
              </p>
            </div>
          )}

          <button
            type="button"
            onClick={() => setShowLogs(value => !value)}
            className="flex w-full items-center justify-between rounded-xl border border-zinc-200 bg-white px-4 py-3 text-sm font-semibold dark:border-white/5 dark:bg-suno-card"
          >
            <span className="flex items-center gap-2">
              <Terminal size={16} />
              {t('engineLog')}
            </span>
            <ChevronDown size={16} className={showLogs ? 'rotate-180 transition-transform' : 'transition-transform'} />
          </button>
          {showLogs && (
            <pre className="max-h-56 overflow-auto rounded-xl border border-zinc-200 bg-zinc-950 p-3 text-[10px] leading-4 text-zinc-300 dark:border-white/10">
              {logs.length ? logs.join('\n') : 'No engine output yet.'}
            </pre>
          )}

          <div className="flex items-center justify-between px-1 text-[11px] text-zinc-500 dark:text-zinc-400">
            <span>
              {t('profile')}: <b className="text-zinc-700 dark:text-zinc-200">{profileLabel}</b>
            </span>
            <button type="button" onClick={() => void refreshSetup().catch((reason: Error) => setError(reason.message))} className="hover:text-pink-500">
              {t('refresh')}
            </button>
          </div>
          {setup?.hardware?.reason && (
            <p className="px-1 text-[10px] text-zinc-400">{setup.hardware.reason}</p>
          )}
          {error && (
            <div role="alert" className="rounded-xl border border-red-500/30 bg-red-500/10 p-3 text-xs leading-5 text-red-700 dark:text-red-200">
              {error}
            </div>
          )}
        </div>
      </div>

      <footer className="shrink-0 border-t border-zinc-200 bg-zinc-50/95 p-4 backdrop-blur dark:border-white/5 dark:bg-suno-panel/95">
        <button
          type="button"
          onClick={submit}
          disabled={activeJobCount >= 10}
          className="flex h-12 w-full items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-orange-500 to-pink-600 text-base font-bold text-white shadow-lg transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {isGenerating ? <Square size={18} /> : <Sparkles size={18} />}
          {t('create') || 'Create'}
          {activeJobCount > 0 && <span className="rounded-full bg-white/20 px-2 py-0.5 text-xs">{activeJobCount}/10</span>}
        </button>
      </footer>
    </section>
  );
};

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block text-xs font-medium text-zinc-600 dark:text-zinc-300">
      <span className="mb-1.5 block">{label}</span>
      {children}
    </label>
  );
}

function NumberField({
  label,
  value,
  min,
  max,
  step,
  onChange,
  disabled,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (value: number) => void;
  disabled?: boolean;
}) {
  return (
    <Field label={label}>
      <input
        type="number"
        value={value}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        onChange={event => {
          const next = Number(event.target.value);
          if (Number.isFinite(next)) onChange(clamp(next, min, max));
        }}
        className={`${CONTROL} disabled:opacity-50`}
      />
    </Field>
  );
}
