import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, CircleAlert, Dices, FolderOpen, Loader2, RotateCcw, Save, Sparkles, Square, Wand2 } from 'lucide-react';
import type { Music3Request, Song } from '../types';
import { useI18n } from '../context/I18nContext';
import { joinCaption, randomExample, splitCaption } from '../services/examples';

/**
 * The Music3 request form.
 *
 * The layout follows what every implementation of this model agrees on — the
 * reference client shipped with the engine, ComfyUI's native nodes and MiniMax's
 * own model card:
 *
 *   * the request is grouped by pipeline stage: prompt, LM configuration, flow
 *     matching, post-processing, components;
 *   * a field left empty means "use the engine default", which is shown as the
 *     placeholder, so the submitted request stays sparse;
 *   * the caption is a structured document — Global Metadata, Vocal Details,
 *     Arrangement — not a one-line prompt, and the lyrics carry bracketed
 *     section tags;
 *   * duration is a *maximum*: the model may end the song earlier.
 *
 * Writing that caption from a one-line idea is a text-LLM job, which this model
 * cannot do — its own language model emits audio codes. Every project solves it
 * with a separate text model, so the assistant here is an optional extra, never
 * the primary way in.
 */

interface CreatePanelProps {
  onGenerate: (request: Music3Request & { _tempId?: string }) => void;
  isGenerating: boolean;
  activeJobCount?: number;
  initialData?: { song: Song; timestamp: number } | null;
}

type EngineDefaults = Partial<Record<string, number | string>>;

type SetupStatus = {
  ready?: boolean;
  engine_ready?: boolean;
  selected_profile_id?: string | null;
  selected_component_ids?: string[] | null;
  effective_max_batch?: number;
  hardware?: { reason?: string };
};

type EngineCatalog = {
  defaults?: EngineDefaults;
  models?: { lm?: string[]; depth?: string[]; cond?: string[]; dit?: string[]; vae?: string[] };
};

/** 9000 acoustic frames at 25 frames per second, as the model card states. */
const MAX_DURATION_SECONDS = 360;
/** The tokenized caption + lyrics budget the engine enforces at submit. */
const MAX_PROMPT_TOKENS = 5000;

const PROFILE_LABEL: Record<string, string> = {
  native: 'Full Native',
  'quality-q8': 'Q8 Quality',
  balanced: 'Balanced',
  'recommended-light': 'Light',
};

const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-zinc-50 px-3 py-2 text-sm text-zinc-900 outline-none transition-colors focus:border-pink-500 disabled:opacity-50 dark:border-white/10 dark:bg-black/25 dark:text-white';
const CARD = 'rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card';
const LABEL = 'mb-1.5 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400';
const TOOL =
  'inline-flex items-center gap-1.5 rounded-lg border border-zinc-200 px-2.5 py-1.5 text-[11px] font-semibold text-zinc-600 transition-colors hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-300';

const numberOrUndefined = (value: string): number | undefined => {
  const trimmed = value.trim();
  if (trimmed === '') return undefined;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : undefined;
};

/** A rough token estimate, only used to warn before the engine rejects it. */
const estimateTokens = (text: string) => Math.ceil(text.trim().length / 3.6);

const Field: React.FC<{ label: string; hint?: string; children: React.ReactNode }> = ({ label, hint, children }) => (
  <label className="block">
    <span className={LABEL}>{label}</span>
    {children}
    {hint && <span className="mt-1 block text-[11px] leading-4 text-zinc-500">{hint}</span>}
  </label>
);

export const CreatePanel: React.FC<CreatePanelProps> = ({ onGenerate, isGenerating, activeJobCount = 0, initialData }) => {
  const { t } = useI18n();

  const [name, setName] = useState('');
  // The caption is three labelled panes, the way the model was trained and the
  // way MiniMax's own demo edits it.
  const [globalMetadata, setGlobalMetadata] = useState('');
  const [vocalDetails, setVocalDetails] = useState('');
  const [arrangement, setArrangement] = useState('');
  const [lyrics, setLyrics] = useState('');
  const [instrumental, setInstrumental] = useState(false);
  const [randomizeSeed, setRandomizeSeed] = useState(true);

  // Parameters are strings so an empty field can mean "engine default".
  const [duration, setDuration] = useState('');
  const [lmBatch, setLmBatch] = useState('');
  const [lmSeed, setLmSeed] = useState('');
  const [lmCfg, setLmCfg] = useState('');
  const [lmTopK, setLmTopK] = useState('');
  const [audioCodes, setAudioCodes] = useState('');
  const [steps, setSteps] = useState('');
  const [ditCfg, setDitCfg] = useState('');
  const [synthBatch, setSynthBatch] = useState('');
  const [seed, setSeed] = useState('');
  const [peakClip, setPeakClip] = useState('');
  const [mp3Bitrate, setMp3Bitrate] = useState('');
  const [format, setFormat] = useState<Music3Request['output_format']>('mp3');
  const [models, setModels] = useState<Record<string, string>>({});

  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [catalog, setCatalog] = useState<EngineCatalog | null>(null);
  const [assistantReady, setAssistantReady] = useState(false);
  const [assisting, setAssisting] = useState<'all' | 'lyrics' | 'prompt' | null>(null);
  const [openSection, setOpenSection] = useState<string | null>(null);
  const [mode, setMode] = useState<'simple' | 'studio'>('studio');
  const [assistInstruction, setAssistInstruction] = useState('');
  const [error, setError] = useState<string | null>(null);
  const promptFile = useRef<HTMLInputElement | null>(null);

  const ready = setup?.ready === true && setup?.engine_ready === true;
  const defaults = catalog?.defaults ?? {};
  const placeholder = (key: string) => (defaults[key] === undefined ? '' : String(defaults[key]));
  const maxSongs = Math.max(1, setup?.effective_max_batch ?? 1);
  const caption = joinCaption(globalMetadata, vocalDetails, arrangement);
  const promptTokens = estimateTokens(caption) + estimateTokens(lyrics);

  const profileLabel = useMemo(() => {
    if (setup?.selected_component_ids?.length) return t('customSet');
    const id = setup?.selected_profile_id;
    return id ? PROFILE_LABEL[id] ?? id : '—';
  }, [setup, t]);

  const refreshSetup = useCallback(async () => {
    const response = await fetch('/setup/status');
    if (!response.ok) throw new Error(String(response.status));
    setSetup(await response.json());
  }, []);

  useEffect(() => { void refreshSetup().catch(() => setSetup(null)); }, [refreshSetup]);

  useEffect(() => {
    void fetch('/v1/local-models/music')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((body: { catalog?: EngineCatalog }) => setCatalog(body.catalog ?? null))
      .catch(() => setCatalog(null));
  }, [setup?.engine_ready]);

  useEffect(() => {
    void fetch('/v1/assistant/status')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((body: { available?: boolean }) => setAssistantReady(body.available === true))
      .catch(() => setAssistantReady(false));
  }, []);

  useEffect(() => {
    if (!initialData?.song) return;
    const song = initialData.song;
    setName(song.title || '');
    const panes = splitCaption(song.style || '');
    setGlobalMetadata(panes.globalMetadata);
    setVocalDetails(panes.vocalDetails);
    setArrangement(panes.arrangement);
    setLyrics(song.lyrics || '');
    const settings = (song.generationParams ?? {}) as EngineDefaults;
    const asString = (key: string) => (settings[key] === undefined ? '' : String(settings[key]));
    setDuration(asString('duration'));
    setSteps(asString('steps'));
    setLmCfg(asString('lm_cfg'));
    setLmTopK(asString('lm_top_k'));
    setDitCfg(asString('dit_cfg'));
    setPeakClip(asString('peak_clip'));
    setMp3Bitrate(asString('mp3_bitrate'));
    if (typeof settings.output_format === 'string') setFormat(settings.output_format as Music3Request['output_format']);
  }, [initialData]);

  const reset = () => {
    setName(''); setGlobalMetadata(''); setVocalDetails(''); setArrangement(''); setLyrics(''); setInstrumental(false);
    setDuration(''); setLmBatch(''); setLmSeed(''); setLmCfg(''); setLmTopK(''); setAudioCodes('');
    setSteps(''); setDitCfg(''); setSynthBatch(''); setSeed('');
    setPeakClip(''); setMp3Bitrate(''); setFormat('mp3'); setModels({});
    setError(null);
  };

  /** One of the official demo prompts bundled with the engine. */
  const loadExample = () => {
    const example = randomExample();
    setGlobalMetadata(example.globalMetadata);
    setVocalDetails(example.vocalDetails);
    setArrangement(example.arrangement);
    setLyrics(example.lyrics);
    setDuration(String(example.duration));
    setName(example.name);
    setError(null);
  };

  const buildRequest = () => {
    const request: Music3Request & { title?: string; audio_codes?: string; models?: Record<string, string> } = {
      caption: caption.trim(),
      lyrics: lyrics.replace(/\r\n?/g, '\n').trim(),
      duration_seconds: Math.min(numberOrUndefined(duration) ?? 60, MAX_DURATION_SECONDS),
      steps: numberOrUndefined(steps) ?? 30,
      seed: randomizeSeed ? undefined : numberOrUndefined(seed),
      lm_seed: numberOrUndefined(lmSeed),
      lm_cfg: numberOrUndefined(lmCfg) ?? 1.5,
      lm_top_k: numberOrUndefined(lmTopK) ?? 50,
      lm_batch_size: Math.min(numberOrUndefined(lmBatch) ?? 1, maxSongs),
      synth_batch_size: numberOrUndefined(synthBatch) ?? 1,
      dit_cfg: numberOrUndefined(ditCfg) ?? 1.7,
      peak_clip: numberOrUndefined(peakClip) ?? 10,
      output_format: format,
      mp3_bitrate: numberOrUndefined(mp3Bitrate) ?? 128,
    };
    if (name.trim()) request.title = name.trim();
    if (audioCodes.trim()) request.audio_codes = audioCodes.trim();
    if (Object.keys(models).length === 5) request.models = models;
    return request;
  };

  const savePrompt = () => {
    const request = buildRequest();
    const blob = new Blob([JSON.stringify(request, null, 2)], { type: 'application/json' });
    const link = document.createElement('a');
    link.href = URL.createObjectURL(blob);
    link.download = `${(name.trim() || 'request').replace(/[\\/:*?"<>|]/g, '')}.json`;
    link.click();
    URL.revokeObjectURL(link.href);
  };

  const openPrompt = async (file: File) => {
    try {
      const parsed = JSON.parse(await file.text()) as Record<string, unknown>;
      const asString = (value: unknown) => (typeof value === 'number' || typeof value === 'string' ? String(value) : '');
      if (typeof parsed.title === 'string') setName(parsed.title);
      if (typeof parsed.caption === 'string') {
        const panes = splitCaption(parsed.caption);
        setGlobalMetadata(panes.globalMetadata);
        setVocalDetails(panes.vocalDetails);
        setArrangement(panes.arrangement);
      }
      if (typeof parsed.lyrics === 'string') setLyrics(parsed.lyrics);
      setDuration(asString(parsed.duration ?? parsed.duration_seconds));
      setSteps(asString(parsed.steps));
      setLmCfg(asString(parsed.lm_cfg));
      setLmTopK(asString(parsed.lm_top_k));
      setLmSeed(asString(parsed.lm_seed));
      setLmBatch(asString(parsed.lm_batch_size));
      setDitCfg(asString(parsed.dit_cfg));
      setSynthBatch(asString(parsed.synth_batch_size));
      setSeed(asString(parsed.seed));
      setPeakClip(asString(parsed.peak_clip));
      setMp3Bitrate(asString(parsed.mp3_bitrate));
      if (typeof parsed.audio_codes === 'string') setAudioCodes(parsed.audio_codes);
      if (typeof parsed.output_format === 'string') setFormat(parsed.output_format as Music3Request['output_format']);
      setError(null);
    } catch {
      setError(t('promptFileInvalid'));
    }
  };

  /// Optional. Nothing here is required to use the model: the manual form is
  /// the primary path, and the buttons stay disabled until a provider is set.
  const askAssistant = async (target: 'all' | 'lyrics' | 'prompt') => {
    if (!assistantReady || assisting) return;
    setAssisting(target);
    setError(null);
    try {
      const response = await fetch('/v1/assistant/write', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          target,
          description: name.trim(),
          instruction: assistInstruction.trim(),
          lyrics: lyrics.trim(),
          global_metadata: globalMetadata.trim(),
          vocal_details: vocalDetails.trim(),
          arrangement: arrangement.trim(),
          duration_seconds: numberOrUndefined(duration) ?? 60,
          instrumental,
        }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || String(response.status));
      if (typeof body?.lyrics === 'string') setLyrics(body.lyrics);
      if (typeof body?.global_metadata === 'string') setGlobalMetadata(body.global_metadata);
      if (typeof body?.vocal_details === 'string') setVocalDetails(body.vocal_details);
      if (typeof body?.arrangement === 'string') setArrangement(body.arrangement);
      setMode('studio');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setAssisting(null);
    }
  };

  const submit = () => {
    if (!ready) { setError(t('downloadProfileFirst')); return; }
    if (!caption.trim()) { setError(t('captionRequired')); return; }
    if (!lyrics.trim()) { setError(t('lyricsRequired')); return; }
    if (promptTokens > MAX_PROMPT_TOKENS) { setError(t('promptTooLong')); return; }
    setError(null);
    onGenerate(buildRequest());
  };

  const totalTracks = (numberOrUndefined(lmBatch) ?? 1) * (numberOrUndefined(synthBatch) ?? 1);
  const roles: Array<{ key: string; label: string; options: string[] }> = [
    { key: 'lm_model', label: 'LM', options: catalog?.models?.lm ?? [] },
    { key: 'depth_model', label: 'Depth', options: catalog?.models?.depth ?? [] },
    { key: 'cond_model', label: 'Cond', options: catalog?.models?.cond ?? [] },
    { key: 'dit_model', label: 'DiT', options: catalog?.models?.dit ?? [] },
    { key: 'vae_model', label: 'VAE', options: catalog?.models?.vae ?? [] },
  ];

  const section = (id: string, title: string, body: React.ReactNode) => (
    <div className={CARD}>
      <button
        type="button"
        onClick={() => setOpenSection(current => (current === id ? null : id))}
        className="flex w-full items-center justify-between text-[11px] font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400"
      >
        {title}
        <ChevronDown size={15} className={openSection === id ? 'rotate-180 transition-transform' : 'transition-transform'} />
      </button>
      {openSection === id && <div className="mt-3 space-y-3">{body}</div>}
    </div>
  );

  return (
    <section className="flex h-full min-h-0 w-full flex-col overflow-hidden bg-zinc-50 text-zinc-900 dark:bg-suno-panel dark:text-white">
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain custom-scrollbar">
        <div className="space-y-3 p-4 pb-6">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h1 className="truncate text-base font-bold">{t('createMusic')}</h1>
              <p className="mt-0.5 truncate text-[11px] text-zinc-500 dark:text-zinc-400">{t('localInference')}</p>
            </div>
            <span className={`shrink-0 rounded-full px-2.5 py-1 text-[10px] font-semibold ${ready ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-700 dark:text-amber-300'}`}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${ready ? 'bg-emerald-500' : 'bg-amber-500'}`} />
              {ready ? t('engineReady') : t('profileRequired')}
            </span>
          </div>

          <div className="flex flex-wrap gap-2">
            <button type="button" onClick={() => promptFile.current?.click()} className={TOOL}><FolderOpen size={13} /> {t('openPrompt')}</button>
            <button type="button" onClick={savePrompt} className={TOOL}><Save size={13} /> {t('savePrompt')}</button>
            <button type="button" onClick={loadExample} className={TOOL}><Dices size={13} /> {t('examplePrompt')}</button>
            <button type="button" onClick={reset} className={TOOL}><RotateCcw size={13} /> {t('resetPrompt')}</button>
            <input
              ref={promptFile}
              type="file"
              accept="application/json,.json"
              className="hidden"
              onChange={event => { const file = event.target.files?.[0]; if (file) void openPrompt(file); event.target.value = ''; }}
            />
          </div>

          {assistantReady && (
            <div className={CARD}>
              <div className="mb-2 grid grid-cols-2 rounded-lg border border-zinc-200 p-1 dark:border-white/10">
                {(['studio', 'simple'] as const).map(value => (
                  <button
                    key={value}
                    type="button"
                    onClick={() => setMode(value)}
                    className={`rounded-md py-1.5 text-[11px] font-semibold transition-colors ${mode === value ? 'bg-zinc-900 text-white dark:bg-white dark:text-zinc-900' : 'text-zinc-500'}`}
                  >
                    {value === 'studio' ? t('studioMode') : t('simpleMode')}
                  </button>
                ))}
              </div>
              {mode === 'simple' ? (
                <>
                  <Field label={t('songIdea')} hint={t('songIdeaHint')}>
                    <textarea value={assistInstruction} onChange={event => setAssistInstruction(event.target.value)} rows={3} placeholder={t('songIdeaPlaceholder')} className={`${CONTROL} resize-y`} />
                  </Field>
                  <button
                    type="button"
                    onClick={() => void askAssistant('all')}
                    disabled={assisting !== null || !assistInstruction.trim()}
                    className="mt-2 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 py-2.5 text-xs font-bold text-white disabled:opacity-50"
                  >
                    {assisting === 'all' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} />}
                    {t('writeEverything')}
                  </button>
                </>
              ) : (
                <p className="text-[11px] leading-4 text-zinc-500">{t('studioModeHint')}</p>
              )}
            </div>
          )}

          {!ready && (
            <div className="flex gap-2 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-xs leading-5 text-amber-800 dark:text-amber-200">
              <CircleAlert className="mt-0.5 shrink-0" size={15} />
              <div><b>{t('localGenerationUnavailable')}</b><br />{t('downloadProfileFirst')}</div>
            </div>
          )}

          <div className={CARD}>
            <Field label={t('title')}>
              <input value={name} onChange={event => setName(event.target.value)} placeholder={t('untitled')} className={CONTROL} />
            </Field>
          </div>

          <div className={CARD}>
            <div className="mb-3 flex items-center justify-between gap-2">
              <span className={LABEL}>{t('captionStructured')}</span>
              <label className="flex cursor-pointer items-center gap-2 text-[11px] font-medium text-zinc-600 dark:text-zinc-300">
                <input type="checkbox" checked={instrumental} onChange={event => setInstrumental(event.target.checked)} className="h-3.5 w-3.5 accent-pink-500" />
                {t('instrumental')}
              </label>
            </div>
            <p className="mb-3 text-[11px] leading-4 text-zinc-500">{t('captionStructuredHint')}</p>

            <div className="space-y-3">
              <Field label={t('globalMetadata')}>
                <textarea value={globalMetadata} onChange={event => setGlobalMetadata(event.target.value)} rows={5} placeholder={t('globalMetadataPlaceholder')} className={`${CONTROL} resize-y leading-5`} />
              </Field>
              <Field label={t('vocalDetails')}>
                <textarea value={vocalDetails} onChange={event => setVocalDetails(event.target.value)} rows={4} placeholder={t('vocalDetailsPlaceholder')} className={`${CONTROL} resize-y leading-5`} />
              </Field>
              <Field label={t('arrangementSection')}>
                <textarea value={arrangement} onChange={event => setArrangement(event.target.value)} rows={5} placeholder={t('arrangementPlaceholder')} className={`${CONTROL} resize-y leading-5`} />
              </Field>
            </div>

            {assistantReady && (
              <button
                type="button"
                onClick={() => void askAssistant('prompt')}
                disabled={assisting !== null}
                className={`${TOOL} mt-3 w-full justify-center py-2 disabled:opacity-50`}
              >
                {assisting === 'prompt' ? <Loader2 size={13} className="animate-spin" /> : <Wand2 size={13} className="text-pink-500" />}
                {t('writeCaption')}
              </button>
            )}
          </div>

          <div className={CARD}>
            <Field label={t('lyrics')} hint={t('lyricsHint')}>
              <textarea
                value={lyrics}
                onChange={event => setLyrics(event.target.value)}
                rows={10}
                placeholder={'[Intro]\n\n[Verse]\n…\n\n[Chorus]\n…'}
                className={`${CONTROL} resize-y font-mono text-xs leading-5`}
              />
            </Field>
            {assistantReady && (
              <button
                type="button"
                onClick={() => void askAssistant('lyrics')}
                disabled={assisting !== null}
                className={`${TOOL} mt-2 w-full justify-center py-2 disabled:opacity-50`}
              >
                {assisting === 'lyrics' ? <Loader2 size={13} className="animate-spin" /> : <Wand2 size={13} className="text-pink-500" />}
                {t('writeLyrics')}
              </button>
            )}
            <p className={`mt-2 text-[11px] ${promptTokens > MAX_PROMPT_TOKENS ? 'text-rose-600 dark:text-rose-300' : 'text-zinc-500'}`}>
              {t('promptBudget')}: ~{promptTokens} / {MAX_PROMPT_TOKENS}
            </p>
          </div>

          <div className={CARD}>
            <span className={LABEL}>{t('lmConfiguration')}</span>
            <div className="grid grid-cols-3 gap-2">
              <Field label={t('maxDuration')}>
                <input value={duration} onChange={event => setDuration(event.target.value)} placeholder={placeholder('duration')} inputMode="decimal" className={CONTROL} />
              </Field>
              <Field label={t('lmBatch')}>
                <input value={lmBatch} onChange={event => setLmBatch(event.target.value)} placeholder={placeholder('lm_batch_size')} inputMode="numeric" disabled={maxSongs === 1} className={CONTROL} />
              </Field>
              <Field label={t('lmSeedShort')}>
                <input value={lmSeed} onChange={event => setLmSeed(event.target.value)} placeholder={placeholder('lm_seed')} inputMode="numeric" className={CONTROL} />
              </Field>
            </div>
            <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('maxDurationHint')}</p>
            {maxSongs === 1 && <p className="mt-1 text-[11px] leading-4 text-zinc-500">{t('maxBatchHint')}</p>}
            {totalTracks > 1 && <p className="mt-1 text-[11px] leading-4 text-zinc-500">{t('renderCountPrefix')} <b>{totalTracks}</b></p>}
          </div>

          {section('advanced-lm', t('advancedLm'), (
            <>
              <div className="grid grid-cols-2 gap-2">
                <Field label={t('cfgScale')}>
                  <input value={lmCfg} onChange={event => setLmCfg(event.target.value)} placeholder={placeholder('lm_cfg')} inputMode="decimal" className={CONTROL} />
                </Field>
                <Field label={t('topK')}>
                  <input value={lmTopK} onChange={event => setLmTopK(event.target.value)} placeholder={placeholder('lm_top_k')} inputMode="numeric" className={CONTROL} />
                </Field>
              </div>
              <Field label={t('audioCodes')} hint={t('audioCodesHint')}>
                <textarea value={audioCodes} onChange={event => setAudioCodes(event.target.value)} rows={3} className={`${CONTROL} resize-y font-mono text-[11px]`} />
              </Field>
            </>
          ))}

          {section('flow', t('flowMatching'), (
            <div className="grid grid-cols-2 gap-2">
              <Field label={t('ditSteps')}>
                <input value={steps} onChange={event => setSteps(event.target.value)} placeholder={placeholder('steps')} inputMode="numeric" className={CONTROL} />
              </Field>
              <Field label={t('cfgScale')}>
                <input value={ditCfg} onChange={event => setDitCfg(event.target.value)} placeholder={placeholder('dit_cfg')} inputMode="decimal" className={CONTROL} />
              </Field>
              <Field label={t('variationsBatch')}>
                <input value={synthBatch} onChange={event => setSynthBatch(event.target.value)} placeholder={placeholder('synth_batch_size')} inputMode="numeric" className={CONTROL} />
              </Field>
              <Field label={t('seedShort')}>
                <input value={seed} onChange={event => setSeed(event.target.value)} placeholder={placeholder('seed')} inputMode="numeric" disabled={randomizeSeed} className={CONTROL} />
                <label className="mt-1.5 flex cursor-pointer items-center gap-2 text-[11px] font-medium text-zinc-600 dark:text-zinc-300">
                  <input type="checkbox" checked={randomizeSeed} onChange={event => setRandomizeSeed(event.target.checked)} className="h-3.5 w-3.5 accent-pink-500" />
                  {t('randomizeSeed')}
                </label>
              </Field>
            </div>
          ))}

          {section('post', t('postProcessing'), (
            <>
              <div className="grid grid-cols-3 gap-2">
                <Field label={t('peakClipLabel')}>
                  <input value={peakClip} onChange={event => setPeakClip(event.target.value)} placeholder={placeholder('peak_clip')} inputMode="numeric" className={CONTROL} />
                </Field>
                <Field label={t('mp3Bitrate')}>
                  <input value={mp3Bitrate} onChange={event => setMp3Bitrate(event.target.value)} placeholder={placeholder('mp3_bitrate')} inputMode="numeric" disabled={format !== 'mp3'} className={CONTROL} />
                </Field>
                <Field label={t('outputFormat')}>
                  <select value={format} onChange={event => setFormat(event.target.value as Music3Request['output_format'])} className={CONTROL}>
                    <option value="mp3">MP3</option>
                    <option value="wav16">WAV16</option>
                    <option value="wav24">WAV24</option>
                    <option value="wav32">WAV32</option>
                  </select>
                </Field>
              </div>
              <p className="text-[11px] leading-4 text-zinc-500">{t('peakClipHint')}</p>
            </>
          ))}

          {section('models', t('componentOverride'), (
            <>
              <p className="text-[11px] leading-4 text-zinc-500">{t('componentOverrideHint')}</p>
              {roles.map(role => (
                <Field key={role.key} label={role.label}>
                  <select
                    value={models[role.key] ?? ''}
                    onChange={event => setModels(current => {
                      const next = { ...current };
                      if (event.target.value) next[role.key] = event.target.value;
                      else delete next[role.key];
                      return next;
                    })}
                    className={CONTROL}
                  >
                    <option value="">{t('profileDefault')}</option>
                    {role.options.map(option => <option key={option} value={option}>{option.replace('MiniMax-Music3-', '')}</option>)}
                  </select>
                </Field>
              ))}
              {Object.keys(models).length > 0 && Object.keys(models).length < 5 && (
                <p className="text-[11px] text-amber-600 dark:text-amber-300">{t('componentOverridePartial')}</p>
              )}
            </>
          ))}

          <div className="flex items-center justify-between px-1 text-[11px] text-zinc-500 dark:text-zinc-400">
            <span>{t('profile')}: <b className="text-zinc-700 dark:text-zinc-200">{profileLabel}</b></span>
            <button type="button" onClick={() => void refreshSetup().catch(() => undefined)} className="hover:text-pink-500">{t('refresh')}</button>
          </div>
          {setup?.hardware?.reason && <p className="px-1 text-[10px] text-zinc-400">{setup.hardware.reason}</p>}
          {error && <div role="alert" className="rounded-xl border border-red-500/30 bg-red-500/10 p-3 text-xs leading-5 text-red-700 dark:text-red-200">{error}</div>}
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
          {t('create')}
          {activeJobCount > 0 && <span className="rounded-full bg-white/20 px-2 py-0.5 text-xs">{activeJobCount}/10</span>}
        </button>
      </footer>
    </section>
  );
};
