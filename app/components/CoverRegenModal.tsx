import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AlertTriangle, Image as ImageIcon, Loader2, Sparkles, Upload, X } from 'lucide-react';
import { Song } from '../types';
import { AlbumCover } from './AlbumCover';

/**
 * Cover art for a library track.
 *
 * Two sources, both real: an image file from disk, or a generation through the
 * catalog-verified OpenRouter image models the native server exposes. The
 * result is stored by the studio server next to the track audio, so the cover
 * belongs to the song rather than living in browser state.
 *
 * ACE Studio generated covers through Pollinations. That path is gone with the
 * ACE backend; this replacement is explicit about cost — nothing is generated
 * until the user presses the button, and the button is disabled until a key and
 * a catalog model exist.
 */

interface CoverRegenModalProps {
  song: Song;
  onClose: () => void;
  onCoverSaved: (songId: string, coverUrl: string) => void;
}

interface CatalogModel {
  id: string;
  name: string;
  capabilities: string[];
}

const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-500 dark:border-white/10 dark:bg-black/20 dark:text-white';

const defaultPrompt = (song: Song) =>
  `Album cover artwork for a track titled "${song.title}". Style: ${song.style || 'contemporary'}. No text, no lettering, square composition.`;

export const CoverRegenModal: React.FC<CoverRegenModalProps> = ({ song, onClose, onCoverSaved }) => {
  const [models, setModels] = useState<CatalogModel[]>([]);
  const [modelId, setModelId] = useState('');
  const [keyConfigured, setKeyConfigured] = useState<boolean | null>(null);
  const [prompt, setPrompt] = useState(() => defaultPrompt(song));
  const [preview, setPreview] = useState<{ dataUrl: string; base64: string; mediaType: string } | null>(null);
  const [busy, setBusy] = useState<'generate' | 'save' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    void fetch('/v1/openrouter/settings')
      .then(response => response.json())
      .then((settings: { configured?: boolean }) => setKeyConfigured(settings.configured === true))
      .catch(() => setKeyConfigured(false));

    void fetch('/v1/openrouter/catalog')
      .then(response => response.json())
      .then((body: { models?: CatalogModel[] }) => {
        const covers = (body.models ?? []).filter(model => model.capabilities.includes('cover_art'));
        setModels(covers);
        setModelId(current => current || covers[0]?.id || '');
      })
      .catch(() => setModels([]));
  }, []);

  const canGenerate = useMemo(() => keyConfigured === true && Boolean(modelId), [keyConfigured, modelId]);

  const generate = useCallback(async () => {
    if (!canGenerate || busy) return;
    setBusy('generate');
    setError(null);
    try {
      const response = await fetch('/v1/openrouter/covers', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model_id: modelId, prompt: prompt.trim() }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || `Cover generation failed (${response.status})`);
      const image = body?.body?.data?.[0];
      if (!image?.b64_json) throw new Error('OpenRouter returned no image data.');
      const mediaType = typeof image.media_type === 'string' ? image.media_type : 'image/png';
      setPreview({ base64: image.b64_json, mediaType, dataUrl: `data:${mediaType};base64,${image.b64_json}` });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Cover generation failed.');
    } finally {
      setBusy(null);
    }
  }, [busy, canGenerate, modelId, prompt]);

  const pickFile = useCallback(async (file: File) => {
    setError(null);
    if (!['image/png', 'image/jpeg', 'image/webp'].includes(file.type)) {
      setError('Use a PNG, JPEG or WebP image.');
      return;
    }
    const buffer = new Uint8Array(await file.arrayBuffer());
    let binary = '';
    for (let offset = 0; offset < buffer.length; offset += 0x8000) {
      binary += String.fromCharCode(...buffer.subarray(offset, offset + 0x8000));
    }
    const base64 = btoa(binary);
    setPreview({ base64, mediaType: file.type, dataUrl: `data:${file.type};base64,${base64}` });
  }, []);

  const save = useCallback(async () => {
    if (!preview || busy) return;
    setBusy('save');
    setError(null);
    try {
      const response = await fetch(`/v1/library/songs/${encodeURIComponent(song.id)}/cover`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ image_base64: preview.base64, media_type: preview.mediaType }),
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error(body?.error || `Storing the cover failed (${response.status})`);
      onCoverSaved(song.id, `/v1/library/songs/${encodeURIComponent(song.id)}/cover`);
      onClose();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Storing the cover failed.');
    } finally {
      setBusy(null);
    }
  }, [busy, onClose, onCoverSaved, preview, song.id]);

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/60 p-4" onClick={onClose}>
      <div className="w-full max-w-lg overflow-hidden rounded-2xl bg-white shadow-2xl dark:bg-zinc-900" onClick={event => event.stopPropagation()}>
        <div className="flex items-center justify-between border-b border-zinc-200 px-5 py-4 dark:border-white/10">
          <h3 className="flex items-center gap-2 text-base font-bold text-zinc-900 dark:text-white">
            <ImageIcon size={18} className="text-pink-500" /> Cover art
          </h3>
          <button type="button" onClick={onClose} className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200">
            <X size={18} />
          </button>
        </div>

        <div className="space-y-4 p-5">
          <div className="flex gap-4">
            <div className="h-28 w-28 shrink-0 overflow-hidden rounded-xl">
              <AlbumCover seed={song.id} size="full" coverUrl={preview?.dataUrl || song.coverUrl} />
            </div>
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-semibold text-zinc-900 dark:text-white">{song.title}</p>
              <p className="mt-0.5 line-clamp-2 text-xs text-zinc-500">{song.style}</p>
              <button
                type="button"
                onClick={() => fileInput.current?.click()}
                className="mt-3 inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-2 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 dark:border-white/15 dark:text-zinc-200"
              >
                <Upload size={13} /> Use an image file
              </button>
              <input
                ref={fileInput}
                type="file"
                accept="image/png,image/jpeg,image/webp"
                className="hidden"
                onChange={event => {
                  const file = event.target.files?.[0];
                  if (file) void pickFile(file);
                  event.target.value = '';
                }}
              />
            </div>
          </div>

          <div className="rounded-xl border border-zinc-200 p-3 dark:border-white/10">
            <label className="text-[11px] font-bold uppercase tracking-wide text-zinc-500">Generate with OpenRouter</label>
            {keyConfigured === false && (
              <p className="mt-2 flex items-start gap-2 text-xs text-amber-600 dark:text-amber-300">
                <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                Add an OpenRouter API key in Settings to generate cover art. Image generation is a paid request.
              </p>
            )}
            {keyConfigured === true && models.length === 0 && (
              <p className="mt-2 flex items-start gap-2 text-xs text-amber-600 dark:text-amber-300">
                <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                The refreshed catalog reports no image-capable model.
              </p>
            )}
            <select value={modelId} onChange={event => setModelId(event.target.value)} disabled={models.length === 0} className={`${CONTROL} mt-2`}>
              {models.length === 0 && <option value="">No image model available</option>}
              {models.map(model => <option key={model.id} value={model.id}>{model.name}</option>)}
            </select>
            <textarea
              value={prompt}
              onChange={event => setPrompt(event.target.value)}
              rows={3}
              className={`${CONTROL} mt-2 resize-none`}
            />
            <button
              type="button"
              onClick={() => void generate()}
              disabled={!canGenerate || busy !== null || !prompt.trim()}
              className="mt-2 inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-4 py-2 text-xs font-bold text-white disabled:cursor-not-allowed disabled:opacity-50"
            >
              {busy === 'generate' ? <Loader2 size={14} className="animate-spin" /> : <Sparkles size={14} />} Generate
            </button>
          </div>

          {error && (
            <p role="alert" className="flex items-center gap-2 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-300">
              <AlertTriangle size={14} /> {error}
            </p>
          )}
        </div>

        <div className="flex justify-end gap-2 border-t border-zinc-200 px-5 py-4 dark:border-white/10">
          <button type="button" onClick={onClose} className="rounded-lg px-4 py-2 text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200">
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={!preview || busy !== null}
            className="inline-flex items-center gap-2 rounded-lg bg-zinc-900 px-4 py-2 text-sm font-bold text-white disabled:opacity-50 dark:bg-white dark:text-zinc-900"
          >
            {busy === 'save' ? <Loader2 size={14} className="animate-spin" /> : null} Save cover
          </button>
        </div>
      </div>
    </div>
  );
};
