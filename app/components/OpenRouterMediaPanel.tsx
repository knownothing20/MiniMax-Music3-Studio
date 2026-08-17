import React, { useEffect, useMemo, useState } from 'react';
import { FileAudio, Image, Loader2, RefreshCw } from 'lucide-react';
import {
  generateCoverWithNativeOpenRouter,
  modelsForCapability,
  refreshNativeOpenRouterCatalog,
  transcribeWithNativeOpenRouter,
  type NativeOpenRouterModel,
} from '../services/nativeOpenRouter';

export function OpenRouterMediaPanel(): React.ReactElement {
  const [models, setModels] = useState<NativeOpenRouterModel[]>([]);
  const [catalogState, setCatalogState] = useState<'idle' | 'loading' | 'ready' | 'error'>('idle');
  const [catalogError, setCatalogError] = useState('');
  const [asrModel, setAsrModel] = useState('');
  const [coverModel, setCoverModel] = useState('');
  const [audioFile, setAudioFile] = useState<File | null>(null);
  const [language, setLanguage] = useState('');
  const [transcript, setTranscript] = useState('');
  const [coverPrompt, setCoverPrompt] = useState('');
  const [coverUrl, setCoverUrl] = useState('');
  const [actionError, setActionError] = useState('');
  const [working, setWorking] = useState<'asr' | 'cover' | null>(null);

  const asrModels = useMemo(() => modelsForCapability(models, 'speech_to_text'), [models]);
  const coverModels = useMemo(() => modelsForCapability(models, 'cover_art'), [models]);

  const refreshCatalog = async () => {
    setCatalogState('loading');
    setCatalogError('');
    try {
      const discovered = await refreshNativeOpenRouterCatalog();
      setModels(discovered);
      setAsrModel((current) => discovered.some((model) => model.id === current && model.capabilities.includes('speech_to_text')) ? current : '');
      setCoverModel((current) => discovered.some((model) => model.id === current && model.capabilities.includes('cover_art')) ? current : '');
      setCatalogState('ready');
    } catch (error) {
      setModels([]);
      setCatalogState('error');
      setCatalogError(error instanceof Error ? error.message : 'The native OpenRouter catalog could not be loaded.');
    }
  };

  useEffect(() => { void refreshCatalog(); }, []);

  const transcribe = async () => {
    if (!audioFile || !asrModel) return;
    setWorking('asr');
    setActionError('');
    try {
      setTranscript(await transcribeWithNativeOpenRouter(asrModel, audioFile, language));
    } catch (error) {
      setActionError(error instanceof Error ? error.message : 'Transcription failed.');
    } finally {
      setWorking(null);
    }
  };

  const generateCover = async () => {
    if (!coverModel || !coverPrompt.trim()) return;
    setWorking('cover');
    setActionError('');
    try {
      setCoverUrl(await generateCoverWithNativeOpenRouter(coverModel, coverPrompt.trim()));
    } catch (error) {
      setActionError(error instanceof Error ? error.message : 'Cover generation failed.');
    } finally {
      setWorking(null);
    }
  };

  return (
    <section className="rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/[0.03]">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold text-zinc-900 dark:text-white">OpenRouter cloud media</h2>
          <p className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">Models are discovered from the native service. Your OpenRouter key is never entered or stored in this UI.</p>
        </div>
        <button onClick={() => void refreshCatalog()} disabled={catalogState === 'loading'} className="inline-flex shrink-0 items-center gap-1 rounded-md border border-zinc-200 px-2 py-1 text-xs text-zinc-700 hover:bg-white disabled:opacity-50 dark:border-white/10 dark:text-zinc-200 dark:hover:bg-white/5">
          <RefreshCw size={12} className={catalogState === 'loading' ? 'animate-spin' : ''} /> Refresh
        </button>
      </div>

      {catalogState === 'error' ? (
        <p className="mt-3 rounded-lg bg-amber-500/10 p-2 text-xs text-amber-700 dark:text-amber-300">Cloud media is unavailable: {catalogError} Start the native music-server, configure its key, then refresh the catalog.</p>
      ) : catalogState === 'loading' || catalogState === 'idle' ? (
        <p className="mt-3 text-xs text-zinc-500">Checking the native OpenRouter catalog…</p>
      ) : (
        <div className="mt-4 grid gap-4 lg:grid-cols-2">
          <div className="space-y-2 rounded-xl border border-zinc-200 p-3 dark:border-white/10">
            <div className="flex items-center gap-2 text-xs font-semibold text-zinc-900 dark:text-white"><FileAudio size={14} className="text-pink-500" /> Transcribe audio</div>
            <select value={asrModel} onChange={(event) => setAsrModel(event.target.value)} className="w-full rounded-md border border-zinc-200 bg-white px-2 py-1.5 text-xs dark:border-white/10 dark:bg-black/20">
              <option value="">{asrModels.length ? 'Choose a discovered ASR model' : 'No ASR models in this catalog'}</option>
              {asrModels.map((model) => <option key={model.id} value={model.id}>{model.name} ({model.id})</option>)}
            </select>
            <input type="file" accept="audio/*" onChange={(event) => setAudioFile(event.target.files?.[0] || null)} className="block w-full text-xs text-zinc-600 dark:text-zinc-400" />
            <input value={language} onChange={(event) => setLanguage(event.target.value)} placeholder="Language, optional (e.g. ru)" className="w-full rounded-md border border-zinc-200 bg-white px-2 py-1.5 text-xs dark:border-white/10 dark:bg-black/20" />
            <button onClick={() => void transcribe()} disabled={!audioFile || !asrModel || working !== null} className="inline-flex items-center gap-1 rounded-md bg-pink-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-pink-700 disabled:opacity-50">
              {working === 'asr' && <Loader2 size={12} className="animate-spin" />} Transcribe
            </button>
            {transcript && <textarea readOnly value={transcript} rows={5} className="w-full rounded-md border border-zinc-200 bg-white p-2 text-xs dark:border-white/10 dark:bg-black/20" aria-label="OpenRouter transcription" />}
          </div>

          <div className="space-y-2 rounded-xl border border-zinc-200 p-3 dark:border-white/10">
            <div className="flex items-center gap-2 text-xs font-semibold text-zinc-900 dark:text-white"><Image size={14} className="text-pink-500" /> Generate cover preview</div>
            <select value={coverModel} onChange={(event) => setCoverModel(event.target.value)} className="w-full rounded-md border border-zinc-200 bg-white px-2 py-1.5 text-xs dark:border-white/10 dark:bg-black/20">
              <option value="">{coverModels.length ? 'Choose a discovered image model' : 'No image models in this catalog'}</option>
              {coverModels.map((model) => <option key={model.id} value={model.id}>{model.name} ({model.id})</option>)}
            </select>
            <textarea value={coverPrompt} onChange={(event) => setCoverPrompt(event.target.value)} rows={3} placeholder="Describe the album cover" className="w-full rounded-md border border-zinc-200 bg-white p-2 text-xs dark:border-white/10 dark:bg-black/20" />
            <button onClick={() => void generateCover()} disabled={!coverModel || !coverPrompt.trim() || working !== null} className="inline-flex items-center gap-1 rounded-md bg-pink-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-pink-700 disabled:opacity-50">
              {working === 'cover' && <Loader2 size={12} className="animate-spin" />} Generate preview
            </button>
            {coverUrl && <img src={coverUrl} alt="OpenRouter cover preview" className="aspect-square w-full max-w-40 rounded-lg object-cover" />}
            <p className="text-[10px] text-zinc-500">Preview only. The retained Pollinations cover workflow remains separate and is not changed by this tool.</p>
          </div>
        </div>
      )}

      {actionError && <p role="alert" className="mt-3 text-xs text-red-600 dark:text-red-400">{actionError}</p>}
    </section>
  );
}
