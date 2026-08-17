import React, { useEffect, useMemo, useState } from 'react';
import { ChevronDown, CircleAlert, Loader2, Music2, Settings2, Sparkles, Square, Wand2 } from 'lucide-react';
import type { GenerationParams, Song } from '../types';

interface CreatePanelProps {
  onGenerate: (params: GenerationParams) => void;
  isGenerating: boolean;
  activeJobCount?: number;
  initialData?: { song: Song; timestamp: number } | null;
}

type MusicJob = {
  id: string;
  status: 'queued' | 'running' | 'completed' | 'failed' | 'cancelled';
  phase: string;
  message: string;
  song?: { id?: string; audio_url?: string };
};

type SetupStatus = {
  ready?: boolean;
  engine_ready?: boolean;
  selected_profile_id?: string | null;
};

const QUALITY = { duration: 60, steps: 30, lmCfg: 1.5, topK: 50, ditCfg: 1.7, lmBatch: 1, synthBatch: 1, peakClip: 10, mp3Bitrate: 128 } as const;
const CONTROL = 'w-full rounded-lg border border-zinc-200 bg-zinc-50 p-2.5 text-sm text-zinc-900 outline-none focus:border-pink-500 dark:border-white/10 dark:bg-black/20 dark:text-white';
const TEMPLATES = [
  { label: 'Поп', caption: 'Modern emotional pop song, polished production, memorable chorus, warm female lead vocal', lyrics: '[Verse]\nI was lost in the city lights\nLooking for a way back home\n\n[Chorus]\nHold on, we are not alone\nTonight our hearts will find the way' },
  { label: 'Электроника', caption: 'Cinematic melodic electronic track, driving four-on-the-floor beat, wide synths, nocturnal atmosphere, male vocal', lyrics: '[Verse]\nNeon on the empty street\nThe night is moving to the beat\n\n[Chorus]\nWe run into the afterglow\nWhere only dreamers ever go' },
  { label: 'Рок', caption: 'Energetic alternative rock, live drums, distorted guitars, anthemic male vocal, dynamic chorus', lyrics: '[Verse]\nDust on my shoes, fire in my veins\nI learned to dance through all the rain\n\n[Chorus]\nTurn it up, let the whole world know\nWe are alive and we will not let go' },
];

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));
const terminal = (job?: MusicJob | null) => Boolean(job && ['completed', 'failed', 'cancelled'].includes(job.status));

export const CreatePanel: React.FC<CreatePanelProps> = ({ activeJobCount = 0, initialData }) => {
  const [mode, setMode] = useState<'simple' | 'advanced'>('simple');
  const [caption, setCaption] = useState('');
  const [lyrics, setLyrics] = useState('');
  const [title, setTitle] = useState('');
  const [duration, setDuration] = useState(QUALITY.duration);
  const [steps, setSteps] = useState(QUALITY.steps);
  const [lmCfg, setLmCfg] = useState(QUALITY.lmCfg);
  const [topK, setTopK] = useState(QUALITY.topK);
  const [ditCfg, setDitCfg] = useState(QUALITY.ditCfg);
  const [format, setFormat] = useState<'mp3' | 'wav16' | 'wav24'>('mp3');
  const [seed, setSeed] = useState<string>('');
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [job, setJob] = useState<MusicJob | null>(null);
  const [error, setError] = useState<string | null>(null);

  const ready = setup?.ready === true && setup?.engine_ready === true;
  const running = Boolean(job && !terminal(job));
  const profileLabel = useMemo(() => {
    if (!setup?.selected_profile_id) return 'профиль не выбран';
    return setup.selected_profile_id === 'recommended-light' ? 'Light · рекомендованный' : setup.selected_profile_id;
  }, [setup]);

  const refreshSetup = async () => {
    const response = await fetch('/setup/status');
    if (!response.ok) throw new Error(`Не удалось проверить локальный движок (${response.status})`);
    setSetup(await response.json());
  };

  useEffect(() => { void refreshSetup().catch((reason: Error) => setError(reason.message)); }, []);

  useEffect(() => {
    if (!initialData?.song) return;
    const song = initialData.song;
    setTitle(song.title || '');
    setCaption(song.style || '');
    setLyrics(song.lyrics || '');
    setMode('advanced');
  }, [initialData]);

  useEffect(() => {
    if (!running || !job) return;
    const timer = window.setInterval(() => {
      void fetch(`/v1/music/jobs/${encodeURIComponent(job.id)}`)
        .then(async response => response.ok ? response.json() : Promise.reject(new Error(`Статус генерации недоступен (${response.status})`)))
        .then((next: MusicJob) => {
          setJob(next);
          if (terminal(next) && next.status === 'completed') window.dispatchEvent(new Event('music3-library-changed'));
        })
        .catch((reason: Error) => setError(reason.message));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [job, running]);

  const chooseTemplate = (template: typeof TEMPLATES[number]) => {
    setCaption(template.caption);
    setLyrics(template.lyrics);
    setMode('advanced');
  };

  const restoreQuality = () => {
    setDuration(QUALITY.duration); setSteps(QUALITY.steps); setLmCfg(QUALITY.lmCfg); setTopK(QUALITY.topK); setDitCfg(QUALITY.ditCfg); setFormat('mp3');
  };

  const submit = async () => {
    if (running) return;
    const cleanCaption = caption.trim();
    const cleanLyrics = lyrics.replace(/\r\n?/g, '\n').trim();
    if (!ready) { setError('Сначала установите и выберите полный профиль Music3 в Менеджере моделей.'); return; }
    if (!cleanCaption) { setError('Добавьте описание/капшен трека.'); return; }
    if (!cleanLyrics) { setError('Music3 требует явный текст песни. Добавьте слова или примените шаблон.'); return; }
    setError(null);
    try {
      const parsedSeed = seed.trim() === '' ? undefined : Number(seed);
      if (parsedSeed !== undefined && (!Number.isInteger(parsedSeed) || parsedSeed < 0)) throw new Error('Seed должен быть целым неотрицательным числом.');
      const response = await fetch('/v1/music/jobs', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ caption: cleanCaption, lyrics: cleanLyrics, duration_seconds: duration, steps, seed: parsedSeed, lm_cfg: lmCfg, lm_top_k: topK, lm_batch_size: QUALITY.lmBatch, synth_batch_size: QUALITY.synthBatch, dit_cfg: ditCfg, peak_clip: QUALITY.peakClip, output_format: format, mp3_bitrate: QUALITY.mp3Bitrate }),
      });
      const next = await response.json().catch(() => null);
      if (!response.ok) throw new Error(next?.message || next?.error || `Не удалось отправить Music3 (${response.status})`);
      setJob(next);
    } catch (reason) { setError(reason instanceof Error ? reason.message : 'Не удалось отправить Music3.'); }
  };

  const cancel = async () => {
    if (!job) return;
    try {
      const response = await fetch(`/v1/music/jobs/${encodeURIComponent(job.id)}`, { method: 'POST' });
      const next = await response.json().catch(() => null);
      if (!response.ok) throw new Error(next?.message || next?.error || 'Не удалось отменить задачу.');
      setJob(next);
    } catch (reason) { setError(reason instanceof Error ? reason.message : 'Не удалось отменить задачу.'); }
  };

  return (
    <section className="flex h-full min-h-0 w-full flex-col overflow-hidden bg-zinc-50 text-zinc-900 dark:bg-suno-panel dark:text-white">
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain custom-scrollbar">
        <div className="space-y-4 p-4 pb-6 pt-4">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h1 className="truncate text-base font-bold">Создать музыку</h1>
              <p className="mt-0.5 truncate text-[11px] text-zinc-500 dark:text-zinc-400">MiniMax Music 3 · локальный C++/CUDA inference</p>
            </div>
            <span className={`shrink-0 rounded-full px-2.5 py-1 text-[10px] font-semibold ${ready ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-700 dark:text-amber-300'}`}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${ready ? 'bg-emerald-500' : 'bg-amber-500'}`} />{ready ? 'Локальный движок готов' : 'Нужен профиль'}
            </span>
          </div>

          <div className="grid grid-cols-2 rounded-xl border border-zinc-200 bg-white p-1 dark:border-white/10 dark:bg-black/20">
            {(['simple', 'advanced'] as const).map(value => <button key={value} type="button" onClick={() => setMode(value)} className={`rounded-lg py-2 text-xs font-semibold transition-colors ${mode === value ? 'bg-zinc-900 text-white shadow-sm dark:bg-white dark:text-zinc-900' : 'text-zinc-500 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-white'}`}>{value === 'simple' ? 'Простой' : 'Ручной'}</button>)}
          </div>

          {!ready && <div className="flex gap-2 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-xs leading-5 text-amber-800 dark:text-amber-200"><CircleAlert className="mt-0.5 shrink-0" size={15} /><div><b>Локальная генерация пока недоступна.</b><br />Выберите и скачайте полный набор компонентов Music3 в Менеджере моделей. Ничего не загружается автоматически.</div></div>}

          {mode === 'simple' ? <div className="space-y-3 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
            <div><label className="text-xs font-bold uppercase tracking-wide text-zinc-500">Идея трека</label><textarea value={caption} onChange={event => setCaption(event.target.value)} placeholder="Например: атмосферный русский synth-pop, ночная поездка, запоминающийся припев" className="mt-2 h-28 w-full resize-none rounded-lg border border-zinc-200 bg-zinc-50 p-3 text-sm outline-none focus:border-pink-500 dark:border-white/10 dark:bg-black/20" /></div>
            <div className="flex items-center justify-between"><span className="text-xs font-bold uppercase tracking-wide text-zinc-500">Быстрый старт</span><Wand2 size={14} className="text-pink-500" /></div>
            <div className="grid grid-cols-3 gap-2">{TEMPLATES.map(template => <button key={template.label} type="button" onClick={() => chooseTemplate(template)} className="rounded-lg border border-zinc-200 px-2 py-2 text-[11px] font-medium hover:border-pink-400 hover:bg-pink-50 dark:border-white/10 dark:hover:bg-pink-500/10">{template.label}</button>)}</div>
            <p className="text-[11px] leading-4 text-zinc-500 dark:text-zinc-400">Music3 генерирует вокальную музыку по явному тексту. Шаблон создаёт редактируемый капшен и слова, ничего не отправляя в сеть.</p>
          </div> : <div className="space-y-3 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
            <Field label="Капшен / стиль"><textarea value={caption} onChange={event => setCaption(event.target.value)} placeholder="Жанр, инструменты, настроение, вокал, аранжировка" className={`${CONTROL} h-24 resize-none`} /></Field>
            <Field label="Текст песни"><textarea value={lyrics} onChange={event => setLyrics(event.target.value)} placeholder={'[Verse]\n...\n\n[Chorus]\n...'} className={`${CONTROL} h-44 resize-none font-mono text-xs`} /></Field>
            <Field label="Название (только для библиотеки)"><input value={title} onChange={event => setTitle(event.target.value)} placeholder="Без названия" className={CONTROL} /></Field>
          </div>}

          {mode === 'simple' && <div className="rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card"><Field label="Текст песни"><textarea value={lyrics} onChange={event => setLyrics(event.target.value)} placeholder="Добавьте слова или примените шаблон выше" className={`${CONTROL} h-32 resize-none font-mono text-xs`} /></Field></div>}

          <div className="rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card">
            <div className="mb-3 flex items-center justify-between"><div className="flex items-center gap-2 text-xs font-bold uppercase tracking-wide text-zinc-500"><Music2 size={14} />Качество</div><button type="button" onClick={restoreQuality} className="text-[11px] font-semibold text-pink-600 hover:text-pink-500">Сбросить эталон</button></div>
            <div className="grid grid-cols-2 gap-3"><NumberField label="Длина, сек." value={duration} min={10} max={300} step={5} onChange={setDuration} /><NumberField label="DiT steps" value={steps} min={2} max={80} step={1} onChange={setSteps} /></div>
            <p className="mt-3 text-[11px] leading-4 text-zinc-500 dark:text-zinc-400">Эталон Light: 60 сек · 30 steps · LM CFG 1.5 · top-k 50 · DiT CFG 1.7.</p>
          </div>

          <button type="button" onClick={() => setShowAdvanced(value => !value)} className="flex w-full items-center justify-between rounded-xl border border-zinc-200 bg-white px-4 py-3 text-sm font-semibold dark:border-white/5 dark:bg-suno-card"><span className="flex items-center gap-2"><Settings2 size={16} />Дополнительно</span><ChevronDown size={16} className={showAdvanced ? 'rotate-180 transition-transform' : 'transition-transform'} /></button>
          {showAdvanced && <div className="space-y-4 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/5 dark:bg-suno-card"><div className="grid grid-cols-2 gap-3"><NumberField label="LM CFG" value={lmCfg} min={0.5} max={4} step={0.1} onChange={setLmCfg} /><NumberField label="LM top-k" value={topK} min={1} max={200} step={1} onChange={setTopK} /><NumberField label="DiT CFG" value={ditCfg} min={0.5} max={5} step={0.1} onChange={setDitCfg} /><Field label="Seed (пусто = случайный)"><input inputMode="numeric" value={seed} onChange={event => setSeed(event.target.value)} placeholder="Случайный" className={CONTROL} /></Field></div><Field label="Формат"><select value={format} onChange={event => setFormat(event.target.value as typeof format)} className={CONTROL}><option value="mp3">MP3</option><option value="wav16">WAV 16-bit</option><option value="wav24">WAV 24-bit</option></select></Field><p className="text-[11px] leading-4 text-zinc-500">Это единственные параметры, которые передаются в локальный Music3 API. Выбор квантов, компонентов и GPU-профиля находится в Менеджере моделей, а не подменяется параметрами ACE.</p></div>}

          <div className="flex items-center justify-between px-1 text-[11px] text-zinc-500 dark:text-zinc-400"><span>Профиль: <b className="text-zinc-700 dark:text-zinc-200">{profileLabel}</b></span><button type="button" onClick={() => void refreshSetup().catch((reason: Error) => setError(reason.message))} className="hover:text-pink-500">Проверить</button></div>
          {job && <div className={`rounded-xl border p-3 text-xs leading-5 ${job.status === 'failed' ? 'border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-200' : job.status === 'completed' ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-200' : 'border-pink-500/30 bg-pink-500/10 text-zinc-700 dark:text-zinc-200'}`}><b>{job.status === 'completed' ? 'Трек готов' : job.status === 'failed' ? 'Генерация не удалась' : 'Music3 работает'}</b><br />{job.message || job.phase}</div>}
          {error && <div role="alert" className="rounded-xl border border-red-500/30 bg-red-500/10 p-3 text-xs leading-5 text-red-700 dark:text-red-200">{error}</div>}
        </div>
      </div>
      <footer className="shrink-0 border-t border-zinc-200 bg-zinc-50/95 p-4 backdrop-blur dark:border-white/5 dark:bg-suno-panel/95"><button type="button" onClick={() => void (running ? cancel() : submit())} disabled={!running && activeJobCount >= 10} className="flex h-12 w-full items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-orange-500 to-pink-600 text-base font-bold text-white shadow-lg transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50">{running ? <Square size={18} /> : <Sparkles size={18} />}{running ? 'Отменить генерацию' : 'Создать Music3 трек'}{activeJobCount > 0 && <span className="rounded-full bg-white/20 px-2 py-0.5 text-xs">{activeJobCount}/10</span>}</button></footer>
    </section>
  );
};

function Field({ label, children }: { label: string; children: React.ReactNode }) { return <label className="block text-xs font-medium text-zinc-600 dark:text-zinc-300"><span className="mb-1.5 block">{label}</span>{children}</label>; }
function NumberField({ label, value, min, max, step, onChange }: { label: string; value: number; min: number; max: number; step: number; onChange: (value: number) => void }) { return <Field label={label}><input type="number" value={value} min={min} max={max} step={step} onChange={event => { const next = Number(event.target.value); if (Number.isFinite(next)) onChange(clamp(next, min, max)); }} className={CONTROL} /></Field>; }
