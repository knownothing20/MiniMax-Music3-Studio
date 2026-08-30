import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { karaokeReason } from '../services/karaoke';
import { AlertTriangle, ChevronDown, CircleAlert, Dices, FolderOpen, Loader2, RotateCcw, Save, Sparkles, Square, Trash2, Wand2, Settings2 } from 'lucide-react';
import type { Music3Request, Song } from '../types';
import { useI18n } from '../context/I18nContext';
import { joinCaption, randomExample, splitCaption } from '../services/examples';
import { GENRE_KEYS } from '../data/genres';
import type { GenerationModePreference } from '../services/studioExecution';
import {
  isWritingAssistantAvailable,
  streamWritingAssistant,
  type WritingAssistantStatus,
  type WritingAssistantDraft,
  type WritingAssistantReceipt,
  type WritingAssistantRequest,
  type WritingAssistantAudit,
  type LyricsStrategy,
} from '../services/writingAssistant';
import {
  appendStyleSuggestion,
  buildAssistantInstruction,
  resolveCaptionRewriterPreference,
  resolveLyricsStrategyPreference,
  MUSIC3_LYRICS_STRATEGY_STORAGE_KEY,
  SIMPLE_CAPTION_REWRITER_STORAGE_KEY,
  useCaptionRewriterForTarget,
  type AssistantTarget,
  type LyricsLanguage,
} from '../services/assistantBrief';

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
  cloudMode?: boolean;
  generationMode: GenerationModePreference;
  onGenerationModeChange: (mode: GenerationModePreference) => void;
  cloudAvailable: boolean;
  localAvailable: boolean;
}

type EngineDefaults = Partial<Record<string, number | string>>;

type AssistantTraceEntry = {
  target: AssistantTarget;
  started_at: string;
  completed_at: string;
  status: 'completed' | 'failed' | 'cancelled';
  request: WritingAssistantRequest;
  visible_stages: string[];
  streamed_output?: string;
  final_draft?: WritingAssistantDraft;
  receipt?: WritingAssistantReceipt;
  audit?: WritingAssistantAudit;
  error?: string;
};

type ProfileFiles = { lm_model: string; depth_model: string; cond_model: string; dit_model: string; vae_model: string };

type SetupStatus = {
  ready?: boolean;
  profile_files?: ProfileFiles | null;
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
/** Exact OmniBridge MiniMax Music route limits (JavaScript length is UTF-16). */
const MAX_CAPTION_CHARACTERS = 1900;
const MAX_LYRICS_CHARACTERS = 3500;

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

const ICON =
  'rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-200 hover:text-black dark:hover:bg-white/10 dark:hover:text-white disabled:opacity-40';

const Field: React.FC<{ label: string; hint?: string; children: React.ReactNode }> = ({ label, hint, children }) => (
  <label className="block">
    <span className={LABEL}>{label}</span>
    {children}
    {hint && <span className="mt-1 block text-[11px] leading-4 text-zinc-500">{hint}</span>}
  </label>
);

/** The toggle used throughout the studio: a real switch, not a tick box. */
const Switch: React.FC<{ checked: boolean; onChange: (value: boolean) => void; label: string; hint?: string }> = ({ checked, onChange, label, hint }) => (
  <div className="flex items-center justify-between gap-3">
    <div className="min-w-0">
      <span className="text-[11px] font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{label}</span>
      {hint && <p className="mt-0.5 text-[11px] leading-4 text-zinc-500">{hint}</p>}
    </div>
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative h-5 w-10 shrink-0 rounded-full transition-colors ${checked ? 'bg-pink-500' : 'bg-zinc-300 dark:bg-zinc-600'}`}
    >
      <span className={`absolute top-[2px] h-4 w-4 rounded-full bg-white shadow-sm transition-all ${checked ? 'left-[22px]' : 'left-[2px]'}`} />
    </button>
  </div>
);

/**
 * A number you drag, with the value beside it.
 *
 * An empty field means "engine default", and that has to survive: the slider
 * shows the default until it is touched, and the reset action puts it back.
 */
const SliderRow: React.FC<{
  label: string;
  value: string;
  fallback: number;
  min: number;
  max: number;
  step: number;
  suffix?: string;
  onChange: (value: string) => void;
  disabled?: boolean;
}> = ({ label, value, fallback, min, max, step, suffix, onChange, disabled }) => {
  const current = value.trim() === '' ? fallback : Number(value);
  const shown = Number.isFinite(current) ? current : fallback;
  const decimals = step < 1 ? String(step).split('.')[1]?.length ?? 1 : 0;
  return (
    <div className={disabled ? 'opacity-50' : undefined}>
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{label}</span>
        <span className="text-[11px] tabular-nums text-zinc-600 dark:text-zinc-300">
          {shown.toFixed(decimals)}{suffix ?? ''}
          {value.trim() === '' && <span className="ml-1 text-zinc-400">·</span>}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={shown}
        disabled={disabled}
        onChange={event => onChange(event.target.value)}
        className="mt-1.5 h-1 w-full cursor-pointer accent-pink-500"
      />
    </div>
  );
};

/** A group inside Advanced: what this stage is, and what these knobs do. */
const Stage: React.FC<{ title: string; hint: string; children: React.ReactNode }> = ({ title, hint, children }) => (
  <section>
    <h4 className="text-[11px] font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{title}</h4>
    <p className="mb-3 mt-0.5 text-[11px] leading-4 text-zinc-500">{hint}</p>
    {children}
  </section>
);

/** A card with a titled header strip, the way the panels are built elsewhere. */
const Card: React.FC<{ title: string; actions?: React.ReactNode; children: React.ReactNode }> = ({ title, actions, children }) => (
  <div className="overflow-hidden rounded-xl border border-zinc-200 bg-white dark:border-white/5 dark:bg-suno-card">
    <div className="flex items-center justify-between gap-2 border-b border-zinc-100 bg-zinc-50 px-3 py-2 dark:border-white/5 dark:bg-white/5">
      <span className="text-[11px] font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{title}</span>
      {actions && <div className="flex items-center gap-1">{actions}</div>}
    </div>
    <div className="p-3">{children}</div>
  </div>
);

/** Grows with its content: these sections run to a thousand characters. */
const AutoTextarea: React.FC<React.TextareaHTMLAttributes<HTMLTextAreaElement> & { minRows?: number }> = ({ minRows = 3, value, ...rest }) => {
  const node = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    const element = node.current;
    if (!element) return;
    element.style.height = 'auto';
    element.style.height = `${Math.max(element.scrollHeight, minRows * 20)}px`;
  }, [value, minRows]);
  return <textarea ref={node} value={value} rows={minRows} {...rest} />;
};

/** One labelled section of the structured caption. */
const Pane: React.FC<{
  label: string;
  value: string;
  placeholder: string;
  onChange: (value: string) => void;
}> = ({ label, value, placeholder, onChange }) => (
  <div className="rounded-lg border border-zinc-200 bg-zinc-50 focus-within:border-pink-500 dark:border-white/10 dark:bg-black/25">
    <div className="flex items-center justify-between px-2.5 pt-2">
      <span className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">{label}</span>
      <span className="text-[10px] tabular-nums text-zinc-400">{value.length}</span>
    </div>
    <AutoTextarea
      value={value}
      minRows={4}
      onChange={event => onChange(event.target.value)}
      placeholder={placeholder}
      className="w-full resize-none bg-transparent px-2.5 pb-2.5 pt-1 text-sm leading-5 text-zinc-900 outline-none dark:text-white"
    />
  </div>
);

export const CreatePanel: React.FC<CreatePanelProps> = ({
  onGenerate,
  isGenerating,
  activeJobCount = 0,
  initialData,
  cloudMode = false,
  generationMode,
  onGenerationModeChange,
  cloudAvailable,
  localAvailable,
}) => {
  const { t, language } = useI18n();

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
  const [durationSource, setDurationSource] = useState<'default' | 'assistant' | 'manual'>('default');
  const [lmSeed, setLmSeed] = useState('');
  const [lmCfg, setLmCfg] = useState('');
  const [lmTopK, setLmTopK] = useState('');
  const [audioCodes, setAudioCodes] = useState('');
  const [steps, setSteps] = useState('');
  const [ditCfg, setDitCfg] = useState('');
  const [synthBatch, setSynthBatch] = useState('');
  // A track read back into the codes the engine renders from. Nothing here
  // changes a request until a file is chosen: no file, no field, and the
  // studio behaves exactly as it did before this existed.
  const [seed, setSeed] = useState('');
  const [peakClip, setPeakClip] = useState('');
  // Quality first, not "quick listen": the engine's own defaults are mp3 at
  // 128 kbps, which throws away what the vocoder produced.
  const [mp3Bitrate, setMp3Bitrate] = useState('320');
  // The engine's own default is 128 kbps, which throws away what the vocoder
  // produced; 320 is the top the encoder offers and costs a few megabytes.
  const [format, setFormat] = useState<Music3Request['output_format']>('mp3');
  const [models, setModels] = useState<Record<string, string>>({});

  const [setup, setSetup] = useState<SetupStatus | null>(null);
  // "Nobody answered" and "the engine says it has no models" are different
  // problems, and telling the user to download 12 GB when the service is simply
  // down is a lie.
  const [serviceDown, setServiceDown] = useState(false);
  const [catalog, setCatalog] = useState<EngineCatalog | null>(null);
  const [assistantReady, setAssistantReady] = useState(false);
  const [assisting, setAssisting] = useState<'all' | 'lyrics' | 'prompt' | null>(null);
  // What the assistant is doing right now, and what it has written so far.
  const [assistStage, setAssistStage] = useState<string | null>(null);
  const [assistModel, setAssistModel] = useState<string | null>(null);
  const [assistDraft, setAssistDraft] = useState('');
  // What the assistant said the cover should show; sent with the request so the
  // automatic cover uses it instead of the generic template.
  const [coverPrompt, setCoverPrompt] = useState('');
  // What the studio is doing to finished tracks: covers and karaoke timings run
  // after generation, and used to run in complete silence.
  const [activity, setActivity] = useState<Array<{ song_id: string; title: string; kind: string; state: string; detail?: string }>>([]);
  useEffect(() => {
    // Finished work changes the track on screen - a cover appears, timings
    // arrive - so the library is told to reread it rather than waiting for the
    // next thing that happens to reload the list.
    let finished = '';
    const read = () => void fetch('/v1/activity')
      .then(response => response.json())
      .then((body: { activity?: typeof activity }) => {
        const entries = body.activity ?? [];
        const done = entries.filter(entry => entry.state === 'done').map(entry => `${entry.song_id}:${entry.kind}`).join(',');
        if (done !== finished) {
          finished = done;
          window.dispatchEvent(new CustomEvent('mm3:library-changed'));
        }
        setActivity(entries);
      })
      .catch(() => undefined);
    read();
    const timer = window.setInterval(read, 2000);
    return () => window.clearInterval(timer);
  }, []);
  // A local model takes tens of seconds to answer. A spinner alone reads as a
  // hung button, so the panel counts the seconds out loud.
  const [assistSeconds, setAssistSeconds] = useState(0);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [mode, setMode] = useState<'simple' | 'studio'>('studio');
  const [captionRewriterEnabled, setCaptionRewriterEnabled] = useState(() => {
    try {
      return resolveCaptionRewriterPreference(localStorage.getItem(SIMPLE_CAPTION_REWRITER_STORAGE_KEY));
    } catch {
      return true;
    }
  });
  const [lyricsStrategy, setLyricsStrategy] = useState<LyricsStrategy>(() => {
    try {
      return resolveLyricsStrategyPreference(localStorage.getItem(MUSIC3_LYRICS_STRATEGY_STORAGE_KEY));
    } catch {
      return 'story_songwriting';
    }
  });
  const [captionRetryPending, setCaptionRetryPending] = useState(false);
  const [assistInstruction, setAssistInstruction] = useState('');
  const [captionInstruction, setCaptionInstruction] = useState('');
  const [lyricsInstruction, setLyricsInstruction] = useState('');
  const [lyricsLanguage, setLyricsLanguage] = useState<LyricsLanguage>('auto');
  const [assistantTrace, setAssistantTrace] = useState<AssistantTraceEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const promptFile = useRef<HTMLInputElement | null>(null);

  const ready = cloudMode || (setup?.ready === true && setup?.engine_ready === true);
  const defaults = catalog?.defaults ?? {};
  const placeholder = (key: string) => (defaults[key] === undefined ? '' : String(defaults[key]));
  const caption = joinCaption(globalMetadata, vocalDetails, arrangement);
  const captionCharacters = caption.length;
  const lyricsCharacters = lyrics.length;

  const profileLabel = useMemo(() => {
    if (setup?.selected_component_ids?.length) return t('customSet');
    const id = setup?.selected_profile_id;
    return id ? PROFILE_LABEL[id] ?? id : '—';
  }, [setup, t]);

  useEffect(() => {
    try {
      localStorage.setItem(SIMPLE_CAPTION_REWRITER_STORAGE_KEY, String(captionRewriterEnabled));
    } catch {
      // A blocked preference store must not stop the assistant from working.
    }
  }, [captionRewriterEnabled]);

  useEffect(() => {
    try {
      localStorage.setItem(MUSIC3_LYRICS_STRATEGY_STORAGE_KEY, lyricsStrategy);
    } catch {
      // Persistence is optional; the active choice still applies to this session.
    }
  }, [lyricsStrategy]);

  useEffect(() => {
    if (!assisting) return;
    setAssistSeconds(0);
    const started = Date.now();
    const timer = window.setInterval(() => setAssistSeconds(Math.round((Date.now() - started) / 1000)), 1000);
    return () => window.clearInterval(timer);
  }, [assisting]);

  const refreshSetup = useCallback(async () => {
    const response = await fetch('/setup/status');
    if (!response.ok) throw new Error(String(response.status));
    setSetup(await response.json());
    setServiceDown(false);
  }, []);

  useEffect(() => {
    if (cloudMode) {
      setSetup(null);
      setServiceDown(false);
      return;
    }
    const poll = () => void refreshSetup().catch(() => { setSetup(null); setServiceDown(true); });
    poll();
    const timer = window.setInterval(poll, 5000);
    return () => window.clearInterval(timer);
  }, [cloudMode, refreshSetup]);

  useEffect(() => {
    if (cloudMode) {
      setCatalog(null);
      return;
    }
    void fetch('/v1/local-models/music')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((body: { catalog?: EngineCatalog }) => setCatalog(body.catalog ?? null))
      .catch(() => setCatalog(null));
  }, [cloudMode, setup?.engine_ready]);

  useEffect(() => {
    // Asked once at mount, the panel kept saying "configure the assistant"
    // long after the assistant had been configured. It is asked again while it
    // is not ready, and whenever the settings are closed.
    const read = () => void fetch('/v1/assistant/status')
      .then(response => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((body: WritingAssistantStatus) => setAssistantReady(isWritingAssistantAvailable(body)))
      .catch(() => setAssistantReady(false));
    read();
    const timer = window.setInterval(read, 5000);
    window.addEventListener('mm3:settings-changed', read);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('mm3:settings-changed', read);
    };
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
    setDurationSource('manual');
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
    setAssistInstruction(''); setCaptionInstruction(''); setLyricsInstruction(''); setLyricsLanguage('auto'); setAssistantTrace([]);
    setDuration(''); setDurationSource('default'); setLmSeed(''); setLmCfg(''); setLmTopK(''); setAudioCodes('');
    setSteps(''); setDitCfg(''); setSynthBatch(''); setSeed('');
    setPeakClip(''); setMp3Bitrate('320'); setFormat('mp3'); setModels({});
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
    setDurationSource('manual');
    setName(example.name);
    setError(null);
  };

  const buildRequest = () => {
    const request: Music3Request & { title?: string; cover_prompt?: string; audio_codes?: string; models?: Record<string, string> } = {
      execution_target: cloudMode ? 'omnibridge' : 'configuration',
      caption: caption.trim(),
      // An instrumental has no words, whatever is still sitting in the box. The
      // lyrics of the previous track stayed there, went to the engine and came
      // back sung: the switch said instrumental and the track had vocals.
      lyrics: instrumental ? '' : lyrics.replace(/\r\n?/g, '\n').trim(),
      duration_seconds: Math.min(numberOrUndefined(duration) ?? 60, MAX_DURATION_SECONDS),
      output_format: format,
    };
    if (!cloudMode) {
      Object.assign(request, {
        steps: numberOrUndefined(steps) ?? 30,
        seed: randomizeSeed ? undefined : numberOrUndefined(seed),
        lm_seed: numberOrUndefined(lmSeed),
        lm_cfg: numberOrUndefined(lmCfg) ?? 1.5,
        lm_top_k: numberOrUndefined(lmTopK) ?? 50,
        lm_batch_size: 1,
        synth_batch_size: numberOrUndefined(synthBatch) ?? 1,
        dit_cfg: numberOrUndefined(ditCfg) ?? 1.7,
        peak_clip: numberOrUndefined(peakClip) ?? 10,
        mp3_bitrate: numberOrUndefined(mp3Bitrate) ?? 128,
      });
    }
    if (name.trim()) request.title = name.trim();
    if (coverPrompt.trim()) request.cover_prompt = coverPrompt.trim();
    if (!cloudMode && audioCodes.trim()) request.audio_codes = audioCodes.trim();
    if (!cloudMode && Object.keys(models).length === 5) request.models = models;
    request.studio_diagnostics = {
      schema_version: 1,
      captured_at: new Date().toISOString(),
      form: {
        mode,
        generation_mode: generationMode,
        briefs: {
          song_idea: assistInstruction.trim(),
          structured_caption: captionInstruction.trim(),
          lyrics: lyricsInstruction.trim(),
          lyrics_language: lyricsLanguage,
        },
        final_copy: {
          title: name.trim(),
          cover_prompt: coverPrompt.trim(),
          global_metadata: globalMetadata.trim(),
          vocal_details: vocalDetails.trim(),
          arrangement: arrangement.trim(),
          lyrics: instrumental ? '' : lyrics.replace(/\r\n?/g, '\n').trim(),
          instrumental,
        },
      },
      assistant_trace: assistantTrace,
    };
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
      setDurationSource('manual');
      setSteps(asString(parsed.steps));
      setLmCfg(asString(parsed.lm_cfg));
      setLmTopK(asString(parsed.lm_top_k));
      setLmSeed(asString(parsed.lm_seed));
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
  // A run in progress can be given up on. The request is a stream that can
  // take minutes on a reasoning model, and without this the only way out of one
  // that went quiet was to close the studio.
  const assistRun = useRef<AbortController | null>(null);
  const stopAssistant = () => {
    assistRun.current?.abort();
    assistRun.current = null;
    setAssisting(null);
    setAssistStage(null);
    setAssistDraft('');
  };
  const askAssistant = async (target: AssistantTarget, instructionOverride?: string) => {
    if (!assistantReady || assisting) return;
    const instruction = instructionOverride?.trim() || buildAssistantInstruction(target, {
      all: assistInstruction,
      prompt: captionInstruction,
      lyrics: lyricsInstruction,
    }, lyricsLanguage, language);
    if (!instruction) return;

    const run = new AbortController();
    assistRun.current = run;
    setAssisting(target);
    setError(null);
    let lyricsStageCompleted = false;

    const runStage = async (payload: WritingAssistantRequest) => {
      const startedAt = new Date().toISOString();
      const visibleStages: string[] = [];
      setAssistStage('preparing');
      setAssistDraft('');
      let streamed = '';
      try {
        const result = await streamWritingAssistant(payload, {
          signal: run.signal,
          onEvent: event => {
            if (event.stage) {
              setAssistStage(event.stage);
              if (visibleStages.at(-1) !== event.stage) visibleStages.push(event.stage);
            }
            if (event.model) setAssistModel(event.model);
            if (event.delta) {
              streamed += event.delta;
              setAssistDraft(streamed);
            }
          },
        });
        setAssistantTrace(previous => [...previous, {
          target: payload.target,
          started_at: startedAt,
          completed_at: new Date().toISOString(),
          status: 'completed',
          request: payload,
          visible_stages: visibleStages,
          streamed_output: result.text || streamed,
          final_draft: result.draft,
          receipt: result.receipt,
          audit: result.audit,
        }]);
        return result;
      } catch (reason) {
        const cancelled = reason instanceof DOMException && reason.name === 'AbortError';
        const message = reason instanceof Error ? reason.message : String(reason);
        setAssistantTrace(previous => [...previous, {
          target: payload.target,
          started_at: startedAt,
          completed_at: new Date().toISOString(),
          status: cancelled ? 'cancelled' : 'failed',
          request: payload,
          visible_stages: visibleStages,
          error: cancelled ? 'Cancelled by the user.' : message,
        }]);
        throw reason;
      }
    };

    const common = {
      description: name.trim(),
      duration_seconds: numberOrUndefined(duration) ?? 60,
      instrumental,
      use_caption_rewriter: captionRewriterEnabled,
      lyrics_strategy: lyricsStrategy,
    };
    const applyLyrics = (body: WritingAssistantDraft) => {
      if (typeof body.lyrics !== 'string') throw new Error('Lyrics stage returned no lyrics.');
      if (body.lyrics.length > MAX_LYRICS_CHARACTERS) throw new Error(t('assistantLyricsTooLong'));
      setLyrics(body.lyrics);
      if (typeof body.title === 'string' && body.title.trim()) setName(body.title.trim());
    };
    const applyCaption = (body: WritingAssistantDraft) => {
      const nextGlobalMetadata = typeof body.global_metadata === 'string' ? body.global_metadata : globalMetadata;
      const nextVocalDetails = typeof body.vocal_details === 'string' ? body.vocal_details : vocalDetails;
      const nextArrangement = typeof body.arrangement === 'string' ? body.arrangement : arrangement;
      if (joinCaption(nextGlobalMetadata, nextVocalDetails, nextArrangement).length > MAX_CAPTION_CHARACTERS) {
        throw new Error(t('assistantCaptionTooLong'));
      }
      if (typeof body.global_metadata === 'string') setGlobalMetadata(nextGlobalMetadata);
      if (typeof body.vocal_details === 'string') setVocalDetails(nextVocalDetails);
      if (typeof body.arrangement === 'string') setArrangement(nextArrangement);
    };

    try {
      if (target === 'all') {
        const lyricsResult = await runStage({
          ...common,
          target: 'lyrics',
          instruction,
          lyrics: '',
          global_metadata: globalMetadata.trim(),
          vocal_details: vocalDetails.trim(),
          arrangement: arrangement.trim(),
        });
        applyLyrics(lyricsResult.draft);
        lyricsStageCompleted = true;
        setCaptionRetryPending(false);

        const generatedLyrics = lyricsResult.draft.lyrics || '';
        const captionResult = await runStage({
          ...common,
          target: 'prompt',
          instruction: assistInstruction.trim(),
          lyrics: generatedLyrics,
          global_metadata: '',
          vocal_details: '',
          arrangement: '',
        });
        applyCaption(captionResult.draft);
      } else {
        const result = await runStage({
          ...common,
          target,
          instruction,
          lyrics: lyrics.trim(),
          global_metadata: globalMetadata.trim(),
          vocal_details: vocalDetails.trim(),
          arrangement: arrangement.trim(),
        });
        if (target === 'lyrics') applyLyrics(result.draft);
        if (target === 'prompt') {
          applyCaption(result.draft);
          setCaptionRetryPending(false);
        }
      }
    } catch (reason) {
      const cancelled = reason instanceof DOMException && reason.name === 'AbortError';
      const message = reason instanceof Error ? reason.message : String(reason);
      if (target === 'all' && lyricsStageCompleted && !cancelled) setCaptionRetryPending(true);
      if (!cancelled) setError(message);
    } finally {
      assistRun.current = null;
      setAssisting(null);
      setAssistStage(null);
      setAssistDraft('');
    }
  };

  const submit = () => {
    if (!ready) { setError(t('downloadProfileFirst')); return; }
    if (!caption.trim()) { setError(t('captionRequired')); return; }
    if (!lyrics.trim()) { setError(t('lyricsRequired')); return; }
    if (captionCharacters > MAX_CAPTION_CHARACTERS) { setError(t('captionTooLong')); return; }
    if (lyricsCharacters > MAX_LYRICS_CHARACTERS) { setError(t('lyricsTooLong')); return; }
    setError(null);
    onGenerate(buildRequest());
  };

  const totalTracks = numberOrUndefined(synthBatch) ?? 1;
  const roles: Array<{ key: string; label: string; options: string[] }> = [
    { key: 'lm_model', label: 'LM', options: catalog?.models?.lm ?? [] },
    { key: 'depth_model', label: 'Depth', options: catalog?.models?.depth ?? [] },
    { key: 'cond_model', label: 'Cond', options: catalog?.models?.cond ?? [] },
    { key: 'dit_model', label: 'DiT', options: catalog?.models?.dit ?? [] },
    { key: 'vae_model', label: 'VAE', options: catalog?.models?.vae ?? [] },
  ];

  const resetParameters = () => {
    setDuration(''); setDurationSource('default'); setLmSeed(''); setLmCfg(''); setLmTopK(''); setAudioCodes('');
    setSteps(''); setDitCfg(''); setSynthBatch(''); setSeed(''); setRandomizeSeed(true);
    setPeakClip(''); setMp3Bitrate('320'); setFormat('mp3'); setModels({});
  };

  const overBudget = captionCharacters > MAX_CAPTION_CHARACTERS || lyricsCharacters > MAX_LYRICS_CHARACTERS;

  return (
    <section className="flex h-full min-h-0 w-full flex-col overflow-hidden bg-zinc-50 text-zinc-900 dark:bg-suno-panel dark:text-white">
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain custom-scrollbar">
        <div className="space-y-3 p-4 pb-6">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h1 className="truncate text-base font-bold">{cloudMode ? `MiniMax Music 3 · ${t('cloudReadyBadge')}` : t('createMusic')}</h1>
              <p className="mt-0.5 truncate text-[11px] text-zinc-500 dark:text-zinc-400">{cloudMode ? t('cloudNoLocalDownload') : t('localInference')}</p>
            </div>
            <span className={`shrink-0 rounded-full px-2.5 py-1 text-[10px] font-semibold ${ready ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-700 dark:text-amber-300'}`}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${ready ? 'bg-emerald-500' : 'bg-amber-500'}`} />
              {cloudMode ? t('cloudReadyBadge') : serviceDown ? t('serviceUnavailable') : ready ? t('engineReady') : t('profileRequired')}
            </span>
          </div>

          <div className="rounded-xl border border-zinc-200 bg-white p-2 dark:border-white/10 dark:bg-suno-card">
            <p className="mb-1.5 px-1 text-[11px] font-semibold text-zinc-500 dark:text-zinc-400">{t('generationSource')}</p>
            <div className="grid grid-cols-3 gap-1 rounded-lg bg-zinc-100 p-1 dark:bg-black/30">
              {(['auto', 'cloud', 'local'] as const).map(value => {
                const available = value === 'auto' || (value === 'cloud' ? cloudAvailable : localAvailable);
                const label = value === 'auto'
                  ? t('generationModeAuto')
                  : value === 'cloud'
                    ? t('generationModeCloud')
                    : t('generationModeLocal');
                const unavailableTitle = value === 'cloud'
                  ? t('cloudApiUnavailable')
                  : value === 'local'
                    ? t('localModelNotInstalled')
                    : undefined;
                return (
                  <button
                    key={value}
                    type="button"
                    disabled={!available}
                    title={!available ? unavailableTitle : undefined}
                    onClick={() => onGenerationModeChange(value)}
                    className={'rounded-md px-2 py-2 text-[11px] font-semibold transition ' + (generationMode === value
                      ? 'bg-white text-pink-600 shadow-sm dark:bg-zinc-800 dark:text-pink-300'
                      : available
                        ? 'text-zinc-600 hover:text-zinc-950 dark:text-zinc-300 dark:hover:text-white'
                        : 'cursor-not-allowed text-zinc-300 dark:text-zinc-600')}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
            {!localAvailable && (
              <p className="mt-1.5 px-1 text-[10px] leading-4 text-zinc-400">{t('localModelNotInstalled')}</p>
            )}
          </div>

          {!cloudMode && (serviceDown ? (
            <div className="flex gap-2 rounded-xl border border-rose-500/30 bg-rose-500/10 p-3 text-xs leading-5 text-rose-700 dark:text-rose-200">
              <CircleAlert className="mt-0.5 shrink-0" size={15} />
              <div><b>{t('serviceUnavailable')}</b><br />{t('serviceUnavailableHint')}</div>
            </div>
          ) : !ready && (
            <div className="flex gap-2 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-xs leading-5 text-amber-800 dark:text-amber-200">
              <CircleAlert className="mt-0.5 shrink-0" size={15} />
              <div><b>{t('localGenerationUnavailable')}</b><br />{t('downloadProfileFirst')}</div>
            </div>
          ))}

          <div className="flex items-center rounded-lg border border-zinc-300 bg-zinc-200 p-1 dark:border-white/5 dark:bg-black/40">
            {(['studio', 'simple'] as const).map(value => (
              <button
                key={value}
                type="button"
                onClick={() => setMode(value)}
                className={`flex-1 rounded-md py-1.5 text-xs font-semibold transition-all ${mode === value ? 'bg-white text-black shadow-sm dark:bg-zinc-800 dark:text-white' : 'text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-300'}`}
              >
                {value === 'studio' ? t('studioMode') : t('simpleMode')}
              </button>
            ))}
          </div>

          <div
            className="w-full max-w-full overflow-hidden rounded-xl border border-zinc-200 bg-zinc-50/80 px-3 py-2.5 dark:border-white/10 dark:bg-white/[0.04]"
            data-testid="caption-rewriter-toolbar"
          >
            <div className="flex min-w-0 items-center gap-2.5">
              <span className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-pink-500/10 text-pink-600 dark:text-pink-300">
                <Sparkles size={15} />
              </span>
              <div className="min-w-0 flex-1 break-words" data-testid="caption-rewriter-copy">
                <p className="break-words text-xs font-bold leading-4 text-zinc-900 dark:text-white">{t('simpleCaptionRewriterLabel')}</p>
                <p className="mt-0.5 break-words text-[10px] leading-4 text-zinc-500 dark:text-zinc-400" data-testid="caption-rewriter-mode">
                  {t(captionRewriterEnabled ? 'captionRewriterActiveMode' : 'captionRewriterStandardMode')}
                </p>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={captionRewriterEnabled}
                aria-label={t('simpleCaptionRewriterLabel')}
                onClick={() => setCaptionRewriterEnabled(enabled => !enabled)}
                className={'relative h-6 w-11 min-w-11 shrink-0 self-center rounded-full transition-colors ' + (captionRewriterEnabled ? 'bg-pink-500' : 'bg-zinc-300 dark:bg-zinc-600')}
              >
                <span className={'absolute left-0.5 top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ' + (captionRewriterEnabled ? 'translate-x-5' : 'translate-x-0')} />
              </button>
            </div>
          </div>

          <div
            className="w-full max-w-full overflow-hidden rounded-xl border border-zinc-200 bg-white px-3 py-2.5 dark:border-white/10 dark:bg-suno-card"
            data-testid="lyrics-strategy-toolbar"
          >
            <div className="flex min-w-0 flex-wrap items-center gap-2 sm:flex-nowrap">
              <div className="min-w-0 flex-1">
                <p className="text-xs font-bold leading-4 text-zinc-900 dark:text-white">{t('lyricsStrategyLabel')}</p>
                <p className="mt-0.5 break-words text-[10px] leading-4 text-zinc-500 dark:text-zinc-400">{t('lyricsStrategyHint')}</p>
              </div>
              <div className="grid w-full shrink-0 grid-cols-2 gap-1 rounded-lg bg-zinc-100 p-1 sm:w-auto" role="group" aria-label={t('lyricsStrategyLabel')}>
                {(['standard', 'story_songwriting'] as LyricsStrategy[]).map(strategy => (
                  <button
                    key={strategy}
                    type="button"
                    aria-pressed={lyricsStrategy === strategy}
                    onClick={() => setLyricsStrategy(strategy)}
                    className={'whitespace-nowrap rounded-md px-2.5 py-1.5 text-[10px] font-semibold transition ' + (lyricsStrategy === strategy
                      ? 'bg-white text-pink-600 shadow-sm dark:bg-zinc-700 dark:text-pink-300'
                      : 'text-zinc-500 hover:text-zinc-900 dark:text-zinc-300')}
                  >
                    {t(strategy === 'standard' ? 'lyricsStrategyStandard' : 'lyricsStrategyStory')}
                  </button>
                ))}
              </div>
            </div>
          </div>

          {mode === 'studio' && (
            <div className="space-y-3" data-testid="music3-assistant-workspace">
              {assistantReady ? (
                <div className="grid gap-3" data-testid="assistant-cards">
                  <section
                    aria-labelledby="caption-assistant-title"
                    className="rounded-xl border border-pink-200 bg-white p-3 shadow-sm dark:border-pink-500/20 dark:bg-suno-card"
                  >
                    <div className="mb-2 flex items-start justify-between gap-2">
                      <div>
                        <h2 id="caption-assistant-title" className="text-sm font-bold text-zinc-900 dark:text-white">{t('structuredAssistantTitle')}</h2>
                        <p className="mt-1 text-[11px] leading-4 text-zinc-500">{t('assistantCaptionOutput')}</p>
                      </div>
                    </div>
                    <label className={LABEL} htmlFor="caption-assistant-brief">{t('assistantCaptionBriefLabel')}</label>
                    <AutoTextarea
                      id="caption-assistant-brief"
                      value={captionInstruction}
                      minRows={2}
                      onChange={event => setCaptionInstruction(event.target.value)}
                      placeholder={t('assistantCaptionBriefPlaceholder')}
                      className={CONTROL + ' resize-none'}
                    />
                    <div className="mt-2" data-testid="caption-style-suggestions">
                      <p className="mb-1.5 text-[10px] font-semibold text-zinc-500">{t('genres')}</p>
                      <div className="flex max-h-20 flex-wrap gap-1.5 overflow-y-auto">
                        {GENRE_KEYS.map(genre => (
                          <button
                            key={genre}
                            type="button"
                            onClick={() => setCaptionInstruction(current => appendStyleSuggestion(current, genre))}
                            className="rounded-full border border-zinc-200 px-2 py-1 text-[10px] font-medium text-zinc-600 transition hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-300"
                          >
                            {genre}
                          </button>
                        ))}
                      </div>
                    </div>
                    <button
                      type="button"
                      onClick={() => void askAssistant('prompt')}
                      disabled={assisting !== null || !captionInstruction.trim()}
                      className="mt-2 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-pink-500 to-orange-500 py-2 text-xs font-bold text-white transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
                    >
                      {assisting === 'prompt' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} />}
                      {assisting === 'prompt' ? t('assistantWritingCaption') : t('writeCaption')}
                    </button>
                  </section>

                  <section
                    aria-labelledby="lyrics-assistant-title"
                    className="rounded-xl border border-orange-200 bg-white p-3 shadow-sm dark:border-orange-500/20 dark:bg-suno-card"
                  >
                    <div className="mb-2 flex items-start justify-between gap-2">
                      <div>
                        <h2 id="lyrics-assistant-title" className="text-sm font-bold text-zinc-900 dark:text-white">{t('lyricsAssistantTitle')}</h2>
                        <p className="mt-1 text-[11px] leading-4 text-zinc-500">{t('assistantLyricsOutput')}</p>
                      </div>
                    </div>
                    <label className={LABEL} htmlFor="lyrics-assistant-brief">{t('assistantLyricsBriefLabel')}</label>
                    <AutoTextarea
                      id="lyrics-assistant-brief"
                      value={lyricsInstruction}
                      minRows={2}
                      onChange={event => setLyricsInstruction(event.target.value)}
                      placeholder={t('assistantLyricsBriefPlaceholder')}
                      className={CONTROL + ' resize-none'}
                    />
                    <div className="mt-2 grid grid-cols-[1fr_auto] items-end gap-2">
                      <Field label={t('lyricsLanguage')}>
                        <select
                          value={lyricsLanguage}
                          onChange={event => setLyricsLanguage(event.target.value as LyricsLanguage)}
                          className={CONTROL}
                        >
                          <option value="auto">{t('lyricsLanguageAuto')}</option>
                          <option value="zh">{t('lyricsLanguageChinese')}</option>
                          <option value="en">{t('lyricsLanguageEnglish')}</option>
                        </select>
                      </Field>
                      <button
                        type="button"
                        onClick={() => void askAssistant('lyrics')}
                        disabled={assisting !== null || !lyricsInstruction.trim()}
                        className="inline-flex h-[38px] items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-500 px-3 text-xs font-bold text-white transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
                      >
                        {assisting === 'lyrics' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} />}
                        {assisting === 'lyrics' ? t('assistantWritingLyrics') : t('writeLyrics')}
                      </button>
                    </div>
                  </section>
                </div>
              ) : (
                <div className="rounded-xl border border-zinc-200 bg-white p-3 dark:border-white/10 dark:bg-suno-card">
                  <div className="grid grid-cols-2 gap-2">
                    <div className="rounded-lg border border-pink-200 bg-pink-50/60 p-2.5 dark:border-pink-500/20 dark:bg-pink-500/5">
                      <p className="text-xs font-bold text-zinc-800 dark:text-zinc-100">{t('structuredAssistantTitle')}</p>
                      <p className="mt-1 text-[10px] leading-4 text-zinc-500">{t('assistantCaptionScope')}</p>
                    </div>
                    <div className="rounded-lg border border-orange-200 bg-orange-50/60 p-2.5 dark:border-orange-500/20 dark:bg-orange-500/5">
                      <p className="text-xs font-bold text-zinc-800 dark:text-zinc-100">{t('lyricsAssistantTitle')}</p>
                      <p className="mt-1 text-[10px] leading-4 text-zinc-500">{t('assistantLyricsScope')}</p>
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => window.dispatchEvent(new CustomEvent('mm3:open-settings', { detail: 'models' }))}
                    className="mt-3 inline-flex w-full items-center justify-center gap-1.5 rounded-lg border border-zinc-300 py-2 text-xs font-bold text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/15 dark:text-zinc-300"
                  >
                    <Settings2 size={13} />
                    {t('setUpAssistant')}
                  </button>
                </div>
              )}
            </div>
          )}

          {mode === 'simple' && !assistantReady && (
            <Card title={t('songIdea')}>
              <p className="text-xs leading-5 text-zinc-500 dark:text-zinc-400">{t('assistantNeedsModel')}</p>
              <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('assistantHint')}</p>
              {/* Telling someone to go to Settings without a way to get there
                  is half an instruction. */}
              <button
                type="button"
                onClick={() => window.dispatchEvent(new CustomEvent('mm3:open-settings', { detail: 'models' }))}
                className="mt-3 inline-flex items-center gap-1 rounded-lg border border-zinc-300 px-3 py-1.5 text-xs font-medium text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/15 dark:text-zinc-300"
              >
                <Settings2 size={13} />
                {t('setUpAssistant')}
              </button>
            </Card>
          )}

          {mode === 'simple' && assistantReady && (
            <Card title={t('songIdea')}>
              <AutoTextarea
                value={assistInstruction}
                minRows={3}
                onChange={event => setAssistInstruction(event.target.value)}
                placeholder={t('songIdeaPlaceholder')}
                className={`${CONTROL} resize-none`}
              />
              <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('songIdeaHint')}</p>
              <Field label={t('lyricsLanguage')}>
                <select
                  value={lyricsLanguage}
                  onChange={event => setLyricsLanguage(event.target.value as LyricsLanguage)}
                  className={CONTROL + ' mt-2'}
                >
                  <option value="auto">{t('lyricsLanguageAuto')}</option>
                  <option value="zh">{t('lyricsLanguageChinese')}</option>
                  <option value="en">{t('lyricsLanguageEnglish')}</option>
                </select>
              </Field>
              <p className="mt-2 rounded-lg bg-zinc-50 px-3 py-2 text-[11px] leading-4 text-zinc-500 dark:bg-black/20">
                {t('assistantFullOutput')}
              </p>
              <button
                type="button"
                onClick={() => void askAssistant('all')}
                disabled={assisting !== null || !assistInstruction.trim()}
                className="mt-3 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 py-2.5 text-xs font-bold text-white transition hover:brightness-110 disabled:opacity-50"
              >
                {assisting === 'all' ? <Loader2 size={14} className="animate-spin" /> : <Wand2 size={14} />}
                {assisting === 'all' ? `${t('assistantWriting')} · ${assistSeconds} ${t('secondsShort')}` : t('writeEverything')}
              </button>

              {captionRetryPending && (
                <button
                  type="button"
                  data-testid="retry-simple-caption"
                  onClick={() => void askAssistant('prompt', assistInstruction)}
                  disabled={assisting !== null}
                  className="mt-2 inline-flex w-full items-center justify-center gap-2 rounded-lg border border-amber-400/60 bg-amber-50 py-2 text-xs font-semibold text-amber-700 transition hover:border-amber-500 dark:bg-amber-500/10 dark:text-amber-200"
                >
                  <RotateCcw size={13} />
                  {t('retryCaptionOnly')}
                </button>
              )}
              {assisting === 'all' && (
                <button
                  type="button"
                  onClick={stopAssistant}
                  className="mt-2 inline-flex w-full items-center justify-center gap-2 rounded-lg border border-zinc-300 py-2 text-xs font-semibold text-zinc-600 transition-colors hover:border-rose-400 hover:text-rose-600 dark:border-white/15 dark:text-zinc-300"
                >
                  <Square size={13} />
                  {t('cancelDownload')}
                </button>
              )}
            </Card>
          )}

          {activity.filter(entry => entry.state !== 'done').slice(-3).map(entry => (
            <div key={`${entry.song_id}-${entry.kind}`} className="rounded-xl border border-zinc-200 bg-white px-3 py-2 text-[11px] dark:border-white/10 dark:bg-suno-card">
              <div className="flex items-center gap-2">
                {entry.state === 'running'
                  ? <Loader2 size={12} className="animate-spin text-pink-500" />
                  : <AlertTriangle size={12} className="text-amber-500" />}
                <span className="font-semibold text-zinc-700 dark:text-zinc-200">
                  {entry.kind === 'cover' ? t('activityCover') : t('activityKaraoke')}
                </span>
                <span className="min-w-0 flex-1 truncate text-zinc-500">{entry.title}</span>
              </div>
              {entry.detail && <p className="mt-1 break-words text-[11px] leading-4 text-amber-600 dark:text-amber-300">{karaokeReason(t, entry.detail)}</p>}
            </div>
          ))}

          {assisting !== null && (
            <div className="rounded-xl border border-zinc-200 bg-white p-3 dark:border-white/10 dark:bg-suno-card">
              <div className="flex items-center justify-between gap-2 text-[11px] font-semibold uppercase tracking-wide">
                <span className="flex items-center gap-1.5 text-pink-600 dark:text-pink-300">
                  <Loader2 size={12} className="animate-spin" />
                  {assistStage === 'preparing' && t('assistStagePreparing')}
                  {assistStage === 'sent' && t('assistStageSent')}
                  {assistStage === 'writing' && t('assistStageWriting')}
                  {assistStage === 'done' && t('assistStageDone')}
                  {!assistStage && t('assistStagePreparing')}
                </span>
                <span className="tabular-nums text-zinc-400">{assistSeconds} {t('secondsShort')}</span>
              </div>
              {assistModel && <p className="mt-1 truncate text-[11px] text-zinc-500">{assistModel}</p>}
              {assistDraft && (
                <pre className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-lg bg-zinc-50 p-2 font-mono text-[11px] leading-4 text-zinc-600 dark:bg-black/30 dark:text-zinc-300">
                  {assistDraft.slice(-1200)}
                </pre>
              )}
            </div>
          )}

          {/* Only for the icon buttons in the card headers: the big button
              already says it in words. */}
          {(assisting === 'lyrics' || assisting === 'prompt') && (
            <div className="flex items-center gap-2 rounded-xl border border-pink-500/30 bg-pink-500/10 px-3 py-2 text-xs text-pink-700 dark:text-pink-200">
              <Loader2 size={14} className="animate-spin" />
              <span>
                {assisting === 'lyrics' ? t('assistantWritingLyrics') : t('assistantWritingCaption')}
                {' · '}{assistSeconds} {t('secondsShort')}
              </span>
            </div>
          )}

          <Card
            title={t('captionStructured')}
            actions={
              <>
                <button type="button" onClick={loadExample} className={ICON} title={t('examplePrompt')}><Dices size={14} /></button>
                <button type="button" onClick={() => promptFile.current?.click()} className={ICON} title={t('openPrompt')}><FolderOpen size={14} /></button>
                <button type="button" onClick={savePrompt} className={ICON} title={t('savePrompt')}><Save size={14} /></button>
                <button type="button" onClick={reset} className={ICON} title={t('resetPrompt')}><RotateCcw size={14} /></button>
                <input
                  ref={promptFile}
                  type="file"
                  accept="application/json,.json"
                  className="hidden"
                  onChange={event => { const file = event.target.files?.[0]; if (file) void openPrompt(file); event.target.value = ''; }}
                />
              </>
            }
          >
            <input
              value={name}
              onChange={event => setName(event.target.value)}
              placeholder={t('untitled')}
              className="w-full border-0 bg-transparent p-0 text-lg font-bold text-zinc-900 outline-none placeholder:text-zinc-300 dark:text-white dark:placeholder:text-zinc-600"
            />
            <p className="mb-3 mt-1 text-[11px] leading-4 text-zinc-500">{t('captionStructuredHint')}</p>
            <div className="mb-3 border-b border-zinc-100 pb-3 dark:border-white/5">
              <Switch checked={instrumental} onChange={setInstrumental} label={t('instrumental')} hint={t('instrumentalHint')} />
            </div>
            <div className="space-y-2">
              <Pane label={t('globalMetadata')} value={globalMetadata} onChange={setGlobalMetadata} placeholder={t('globalMetadataPlaceholder')} />
              <Pane label={t('vocalDetails')} value={vocalDetails} onChange={setVocalDetails} placeholder={t('vocalDetailsPlaceholder')} />
              <Pane label={t('arrangementSection')} value={arrangement} onChange={setArrangement} placeholder={t('arrangementPlaceholder')} />
            </div>
          </Card>

          <Card
            title={t('lyrics')}
            actions={
              <>
                <span
                  className={`rounded-full px-2 py-0.5 text-[10px] font-semibold tabular-nums ${overBudget ? 'bg-rose-500/10 text-rose-600 dark:text-rose-300' : 'bg-zinc-200/70 text-zinc-500 dark:bg-white/10 dark:text-zinc-400'}`}
                  title={`${t('promptBudget')} — ${t('caption')}: ${captionCharacters}/${MAX_CAPTION_CHARACTERS}, ${t('lyrics')}: ${lyricsCharacters}/${MAX_LYRICS_CHARACTERS}`}
                >
                  {t('caption')} {captionCharacters}/{MAX_CAPTION_CHARACTERS} · {t('lyrics')} {lyricsCharacters}/{MAX_LYRICS_CHARACTERS}
                </span>
                <button type="button" onClick={() => setLyrics('')} className={ICON} title={t('resetPrompt')}><RotateCcw size={14} /></button>
              </>
            }
          >
            <AutoTextarea
              value={lyrics}
              minRows={10}
              onChange={event => setLyrics(event.target.value)}
              placeholder={'[intro]\n\n[verse]\n…\n\n[chorus]\n…'}
              className={`${CONTROL} resize-none font-mono text-xs leading-5`}
            />
            <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('lyricsHint')}</p>
            {captionCharacters > MAX_CAPTION_CHARACTERS && <p className="mt-1 text-[11px] leading-4 text-rose-600 dark:text-rose-300">{t('captionTooLong')}</p>}
            {lyricsCharacters > MAX_LYRICS_CHARACTERS && <p className="mt-1 text-[11px] leading-4 text-rose-600 dark:text-rose-300">{t('lyricsTooLong')}</p>}
          </Card>

          <Card
            title={t('quality')}
            actions={
              <button type="button" onClick={resetParameters} className="rounded-md px-2 py-1 text-[10px] font-semibold text-zinc-500 transition-colors hover:bg-zinc-200 hover:text-black dark:hover:bg-white/10 dark:hover:text-white">
                {t('resetToDefaults')}
              </button>
            }
          >
            <div className="space-y-3">
              {cloudMode ? (
                <div className="rounded-xl border border-sky-200 bg-sky-50 px-3 py-2 text-[11px] leading-5 text-sky-800 dark:border-sky-900/60 dark:bg-sky-950/30 dark:text-sky-200">
                  {t('cloudDurationNotControlled')}
                </div>
              ) : (
                <>
                  <SliderRow
                    label={t('maxDuration')}
                    value={duration}
                    fallback={Number(defaults.duration ?? 60)}
                    min={10}
                    max={MAX_DURATION_SECONDS}
                    step={5}
                    suffix=" s"
                    onChange={value => { setDuration(value); setDurationSource('manual'); }}
                  />
                  <p className="text-[11px] leading-4 text-zinc-500">{t('maxDurationHint')}</p>
                  <p className="text-[11px] leading-4 text-zinc-500">{t(durationSource === 'assistant' ? 'durationSourceAssistant' : durationSource === 'manual' ? 'durationSourceManual' : 'durationSourceDefault')}</p>
                </>
              )}
              {!cloudMode && (
                <>
                  <SliderRow
                    label={t('ditSteps')}
                    value={steps}
                    fallback={Number(defaults.steps ?? 30)}
                    min={8}
                    max={80}
                    step={1}
                    onChange={setSteps}
                  />
                  <SliderRow
                    label={t('cfgScale')}
                    value={ditCfg}
                    fallback={Number(defaults.dit_cfg ?? 1.7)}
                    min={1}
                    max={5}
                    step={0.1}
                    onChange={setDitCfg}
                  />
                </>
              )}
            </div>

            {!cloudMode && <div className="mt-4 space-y-3 border-t border-zinc-100 pt-4 dark:border-white/5">
              {/* No batch slider. The engine reserves KV cache for the whole
                  batch when it loads its weights and takes the number only as a
                  launch flag, so a control here could not change anything about
                  the run it appears in. */}
              <SliderRow
                label={t('variationsBatch')}
                value={synthBatch}
                fallback={Number(defaults.synth_batch_size ?? 1)}
                min={1}
                max={4}
                step={1}
                onChange={setSynthBatch}
              />
              <Switch checked={randomizeSeed} onChange={setRandomizeSeed} label={t('randomizeSeed')} />
              {!randomizeSeed && (
                <Field label={t('seedShort')}>
                  <input value={seed} onChange={event => setSeed(event.target.value)} placeholder={placeholder('seed')} inputMode="numeric" className={CONTROL} />
                </Field>
              )}
              {totalTracks > 1 && (
                <p className="text-[11px] text-zinc-500">{t('renderCountPrefix')} <b className="text-zinc-700 dark:text-zinc-200">{totalTracks}</b></p>
              )}
            </div>}
          </Card>

          {!cloudMode && <div className="overflow-hidden rounded-xl border border-zinc-200 bg-white dark:border-white/5 dark:bg-suno-card">
            <button
              type="button"
              onClick={() => setShowAdvanced(current => !current)}
              className="flex w-full items-center justify-between gap-2 px-3 py-2 text-[11px] font-bold uppercase tracking-wide text-zinc-500 transition-colors hover:text-black dark:text-zinc-400 dark:hover:text-white"
            >
              {t('advanced')}
              <ChevronDown size={15} className={showAdvanced ? 'rotate-180 transition-transform' : 'transition-transform'} />
            </button>
            {showAdvanced && (
              <div className="space-y-4 border-t border-zinc-100 p-3 dark:border-white/5">
                <Stage title={t('stageLm')} hint={t('stageLmHint')}>
                  <div className="space-y-3">
                    <SliderRow
                      label={t('cfgScale')}
                      value={lmCfg}
                      fallback={Number(defaults.lm_cfg ?? 1.5)}
                      min={1}
                      max={5}
                      step={0.1}
                      onChange={setLmCfg}
                    />
                    <SliderRow
                      label={t('topK')}
                      value={lmTopK}
                      fallback={Number(defaults.lm_top_k ?? 50)}
                      min={1}
                      max={200}
                      step={1}
                      onChange={setLmTopK}
                    />
                    <Field label={t('lmSeedShort')}>
                      <input value={lmSeed} onChange={event => setLmSeed(event.target.value)} placeholder={placeholder('lm_seed')} inputMode="numeric" className={CONTROL} />
                    </Field>
                  </div>
                </Stage>

                <div className="border-t border-zinc-100 pt-4 dark:border-white/5">
                  <Stage title={t('stageOutput')} hint={t('stageOutputHint')}>
                  <SliderRow
                    label={t('peakClipLabel')}
                    value={peakClip}
                    fallback={Number(defaults.peak_clip ?? 10)}
                    min={0}
                    max={30}
                    step={1}
                    onChange={setPeakClip}
                  />
                  <div className="mt-3 grid grid-cols-2 gap-2">
                    <Field label={t('mp3Bitrate')}>
                      <select value={mp3Bitrate || String(defaults.mp3_bitrate ?? 128)} onChange={event => setMp3Bitrate(event.target.value)} disabled={format !== 'mp3'} className={CONTROL}>
                        {['128', '192', '256', '320'].map(rate => <option key={rate} value={rate}>{rate} kbps</option>)}
                      </select>
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
                  <p className="mt-2 text-[11px] leading-4 text-zinc-500">{t('peakClipHint')}</p>
                  </Stage>
                </div>

                <div className="border-t border-zinc-100 pt-4 dark:border-white/5">
                  <Stage title={t('componentOverride')} hint={t('componentOverrideHint')}>
                  <div className="space-y-2">
                    {roles.map(role => (
                      <div key={role.key} className="grid grid-cols-[64px_1fr] items-center gap-2">
                        <span className="text-[11px] font-semibold text-zinc-500 dark:text-zinc-400">{role.label}</span>
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
                          <option value="">
                            {(() => {
                              // Name the file the profile actually loads: "profile
                              // default" beside every role told the user nothing.
                              const inUse = setup?.profile_files?.[role.key as keyof ProfileFiles];
                              return inUse ? `${t('profileDefault')} · ${inUse.replace('MiniMax-Music3-', '')}` : t('profileDefault');
                            })()}
                          </option>
                          {role.options.map(option => <option key={option} value={option}>{option.replace('MiniMax-Music3-', '')}</option>)}
                        </select>
                      </div>
                    ))}
                  </div>
                  {Object.keys(models).length > 0 && Object.keys(models).length < 5 && (
                    <p className="mt-2 text-[11px] text-amber-600 dark:text-amber-300">{t('componentOverridePartial')}</p>
                  )}
                  </Stage>
                </div>
              </div>
            )}
          </div>}

          {!cloudMode && <div className="flex items-center justify-between px-1 text-[11px] text-zinc-500 dark:text-zinc-400">
            <button
              type="button"
              onClick={() => window.dispatchEvent(new CustomEvent('mm3:open-settings', { detail: 'models' }))}
              className="text-left hover:text-pink-500"
              title={t('changeProfileHint')}
            >
              {t('profile')}: <b className="text-zinc-700 underline decoration-dotted underline-offset-2 dark:text-zinc-200">{profileLabel}</b>
            </button>
            <button type="button" onClick={() => void refreshSetup().catch(() => undefined)} className="hover:text-pink-500">{t('refresh')}</button>
          </div>}
          {!cloudMode && setup?.hardware?.reason && <p className="px-1 text-[10px] text-zinc-400">{setup.hardware.reason}</p>}
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
