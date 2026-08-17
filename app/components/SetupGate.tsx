import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Check, Download, Loader2, Square, X } from 'lucide-react';
import {
  componentKindLabel,
  componentPrecision,
  componentsByKind,
  completeCustomComponentIds,
  selectedComponentBytes,
  type Music3Component,
} from '../services/music3ModelCatalog';

type DownloadJob = {
  id: string;
  profile_id: string | null;
  component_ids: string[];
  status: 'downloading' | 'completed' | 'cancelled' | 'failed';
  downloaded_bytes: number;
  total_bytes: number;
  error: string | null;
};

type SetupStatus = {
  ready: boolean;
  hardware?: { gpuName?: string; totalVramGb?: number; recommended?: string; reason?: string };
  engine_ready: boolean;
  engine_id: string;
  first_run: boolean;
  download_pending: number;
  recommended_profile_id: string;
  active: DownloadJob | null;
  installed_components: string[];
};

type Profile = {
  id: string;
  label: string;
  backend: string;
  installable: boolean;
  recommended: boolean;
  components: string[];
  total_bytes: number;
};

type Catalog = { engine_id: string; recommended_profile_id: string; profiles: Profile[]; components: Music3Component[] };
type EnginePreset = { id: string; title: string; subtitle?: string };
type PresetsResponse = { presets: EnginePreset[]; hardware?: { recommended?: string } };
type CapabilitiesResponse = { engines: Array<{ id?: string; execution_mode?: string; capabilities?: string[] }> };

const bytes = (value: number) => {
  if (value < 1024 ** 3) return `${Math.round(value / 1024 ** 2)} MB`;
  return `${(value / 1024 ** 3).toFixed(1)} GB`;
};

const errorMessage = async (response: Response) => {
  const body = await response.json().catch(() => null);
  return body?.error || `Request failed (${response.status})`;
};

export const SetupGate: React.FC<{ onReady: () => void }> = ({ onReady }) => {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [selectedProfile, setSelectedProfile] = useState('');
  const [advanced, setAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [presets, setPresets] = useState<PresetsResponse | null>(null);
  const [presetId, setPresetId] = useState('');
  const [presetBusy, setPresetBusy] = useState(false);
  const [openRouterMusicReady, setOpenRouterMusicReady] = useState<boolean | null>(null);
  const [customComponents, setCustomComponents] = useState<Record<string, string>>({});

  const refresh = useCallback(async () => {
    const [statusResponse, catalogResponse] = await Promise.all([
      fetch('/setup/status'),
      fetch('/setup/catalog'),
    ]);
    if (!statusResponse.ok) throw new Error(await errorMessage(statusResponse));
    if (!catalogResponse.ok) throw new Error(await errorMessage(catalogResponse));
    let nextStatus: SetupStatus = await statusResponse.json();
    const nextCatalog: Catalog = await catalogResponse.json();
    if (nextStatus.ready && !nextStatus.engine_ready) {
      // The service starts its own engine. Going through a Tauri command here
      // meant the studio could not start generation when it was opened in a
      // browser, and the failure surfaced as "cannot read properties of
      // undefined (reading 'invoke')".
      const startResponse = await fetch('/engine/start', { method: 'POST' });
      if (!startResponse.ok) throw new Error(await errorMessage(startResponse));
      const engineResponse = await fetch('/setup/status');
      if (!engineResponse.ok) throw new Error(await errorMessage(engineResponse));
      nextStatus = await engineResponse.json();
      if (!nextStatus.engine_ready) throw new Error(`The local ${nextStatus.engine_id} runtime did not become ready.`);
    }
    setStatus(nextStatus);
    setCatalog(nextCatalog);
    setSelectedProfile((current) => current || nextStatus.recommended_profile_id);
    setCustomComponents((current) => {
      if (Object.keys(current).length > 0) return current;
      const recommended = nextCatalog.profiles.find((profile) => profile.id === nextCatalog.recommended_profile_id);
      return (recommended?.components || []).reduce<Record<string, string>>((selection, id) => {
        const component = nextCatalog.components.find((candidate) => candidate.id === id);
        if (component) selection[component.kind] = component.id;
        return selection;
      }, {});
    });
    if (nextStatus.ready && nextStatus.engine_ready) onReady();
  }, [onReady]);

  useEffect(() => { void refresh().catch((reason) => setError(reason.message)); }, [refresh]);
  useEffect(() => {
    void fetch('/engine/presets')
      .then(async (response) => response.ok ? response.json() : Promise.reject(new Error(await errorMessage(response))))
      .then((next: PresetsResponse) => {
        setPresets(next);
        setPresetId(next.hardware?.recommended || next.presets[0]?.id || '');
      })
      .catch(() => setPresets({ presets: [] }));
  }, []);
  useEffect(() => {
    void fetch('/v1/capabilities')
      .then(async (response) => response.ok ? response.json() : Promise.reject(new Error(await errorMessage(response))))
      .then((response: CapabilitiesResponse) => {
        const openRouter = response.engines.find((engine) => engine.id === 'openrouter' && engine.execution_mode === 'open_router');
        setOpenRouterMusicReady(openRouter?.capabilities?.includes('music_generation') === true);
      })
      .catch(() => setOpenRouterMusicReady(null));
  }, []);
  useEffect(() => {
    if (!status?.active || status.active.status !== 'downloading') return;
    const timer = window.setInterval(() => void refresh().catch((reason) => setError(reason.message)), 1000);
    return () => window.clearInterval(timer);
  }, [refresh, status?.active]);

  const installableProfiles = useMemo(
    () => catalog?.profiles.filter((profile) => profile.installable) ?? [],
    [catalog],
  );
  const selected = installableProfiles.find((profile) => profile.id === selectedProfile);
  const componentGroups = useMemo(() => componentsByKind(catalog?.components || []), [catalog]);
  const selectedCustomIds = useMemo(() => completeCustomComponentIds(catalog?.components || [], customComponents), [catalog, customComponents]);
  const selectedCustomBytes = useMemo(() => selectedComponentBytes(catalog?.components || [], selectedCustomIds), [catalog, selectedCustomIds]);
  const active = status?.active;
  const progress = active && active.total_bytes > 0 ? Math.min(100, (active.downloaded_bytes / active.total_bytes) * 100) : 0;

  const download = async () => {
    if ((!selected && !selectedCustomIds) || active?.status === 'downloading') return;
    setError(null);
    const payload = selected ? { profile_id: selected.id } : { ids: selectedCustomIds! };
    const response = await fetch('/setup/download', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    if (!response.ok) {
      setError(await errorMessage(response));
      return;
    }
    await refresh();
  };

  const cancel = async () => {
    const response = await fetch('/setup/cancel', { method: 'POST' });
    if (!response.ok) {
      setError(await errorMessage(response));
      return;
    }
    await refresh();
  };

  const applyPreset = async () => {
    if (!presetId || presetBusy) return;
    setPresetBusy(true);
    const response = await fetch('/engine/preset', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: presetId }),
    });
    setPresetBusy(false);
    if (!response.ok) setError(await errorMessage(response));
  };

  return (
    <div className="flex h-full w-full items-center justify-center overflow-y-auto bg-white px-5 py-10 dark:bg-suno">
      <div className="w-full max-w-2xl">
        <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-[0.18em] text-pink-500">
          <span className="h-2 w-2 rounded-full bg-pink-500 shadow-[0_0_10px_rgba(236,72,153,0.75)]" />
          First run
        </div>
        <h1 className="mt-4 text-3xl font-extrabold tracking-tight text-zinc-900 dark:text-white">Set up local music generation</h1>
        <p className="mt-3 max-w-xl text-sm leading-6 text-zinc-500 dark:text-zinc-400">Download one complete, verified five-component minimaxmusic.cpp profile. The advanced builder requires exactly one compatible LM, depth decoder, condition encoder, DiT, and vocoder; the native backend rejects partial sets.</p>

        {error && <div className="mt-5 flex gap-2 rounded-xl border border-rose-300 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-500/30 dark:bg-rose-500/10 dark:text-rose-200"><X size={16} className="mt-0.5 shrink-0" />{error}</div>}

        <div className="mt-5 rounded-xl border border-pink-300/50 bg-pink-50 px-4 py-3 text-sm text-zinc-700 dark:border-pink-500/25 dark:bg-pink-500/10 dark:text-zinc-200">
          <span className="font-semibold text-pink-600 dark:text-pink-400">Recommended for this machine:</span>{' '}
          {status?.hardware?.reason || 'the profile matching the detected GPU is preselected'}. Downloads are resumable and every
          component is checksum-verified before generation is enabled.
        </div>

        {status?.ready && !status.engine_ready && <div className="mt-5 rounded-xl border border-amber-300 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-200">
          Model files are installed, but the local {status.engine_id} runtime is not ready. Studio will start it automatically after the selected profile has been verified.
        </div>}

        {presets && presets.presets.length > 0 && <div className="mt-5 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-suno-card">
          <div className="flex items-center justify-between gap-3"><div><div className="text-xs font-bold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">Hardware preset</div><p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">Applies the native server preset for this machine.</p></div>{presets.hardware?.recommended && <span className="rounded-full bg-pink-500/10 px-2 py-1 text-[10px] font-bold uppercase tracking-wide text-pink-600 dark:text-pink-300">Recommended</span>}</div>
          <div className="mt-3 flex gap-2"><select value={presetId} onChange={(event) => setPresetId(event.target.value)} disabled={presetBusy || !!active} className="min-w-0 flex-1 rounded-lg border border-zinc-200 bg-zinc-50 px-3 py-2 text-sm text-zinc-900 focus:border-pink-500 focus:outline-none dark:border-white/10 dark:bg-black/20 dark:text-white">{presets.presets.map((preset) => <option key={preset.id} value={preset.id}>{preset.title}{preset.subtitle ? ` — ${preset.subtitle}` : ''}</option>)}</select><button type="button" onClick={() => void applyPreset()} disabled={!presetId || presetBusy || !!active} className="rounded-lg border border-zinc-300 px-3 text-xs font-bold text-zinc-700 hover:border-pink-400 hover:text-pink-600 disabled:opacity-50 dark:border-white/15 dark:text-zinc-200">{presetBusy ? 'Applying…' : 'Apply'}</button></div>
          <p className={`mt-3 text-[11px] ${openRouterMusicReady ? 'text-emerald-700 dark:text-emerald-300' : 'text-amber-700 dark:text-amber-300'}`}>{openRouterMusicReady ? 'A Full OpenRouter preset can enable Music: the server resolved a music-capable OpenRouter provider.' : 'OpenRouter Music requires a catalog refresh: no resolved music-capable provider is currently reported by the server.'}</p>
        </div>}

        <div className="mt-5 space-y-3">
          {installableProfiles.filter((profile) => advanced || profile.recommended).map((profile) => {
            const checked = profile.id === selectedProfile;
            const profileComponents = profile.components.map((id) => catalog?.components.find((component) => component.id === id)).filter((component): component is Music3Component => Boolean(component));
            return <button key={profile.id} type="button" onClick={() => !active && setSelectedProfile(profile.id)} disabled={!!active}
              className={`w-full rounded-xl border p-4 text-left transition-colors ${checked ? 'border-pink-500 bg-pink-50 dark:bg-pink-500/10' : 'border-zinc-200 bg-white hover:border-pink-300 dark:border-white/10 dark:bg-suno-card'} disabled:cursor-not-allowed`}>
              <div className="flex items-center gap-3">
                <span className={`grid h-5 w-5 place-items-center rounded-full border ${checked ? 'border-pink-500 bg-pink-500 text-white' : 'border-zinc-300 dark:border-zinc-600'}`}>{checked && <Check size={13} strokeWidth={3} />}</span>
                <span className="min-w-0 flex-1 font-semibold text-zinc-900 dark:text-white">{profile.label}</span>
                <span className="shrink-0 text-xs text-zinc-500 dark:text-zinc-400">{bytes(profile.total_bytes)}</span>
              </div>
              <div className="ml-8 mt-1 text-xs text-zinc-500 dark:text-zinc-400">{profileComponents.map((component) => `${componentKindLabel(component.kind)} ${componentPrecision(component)}`).join(' · ')} · {profile.backend}</div>
            </button>;
          })}
          {advanced && <button type="button" onClick={() => !active && setSelectedProfile('custom')} disabled={!!active}
            className={`w-full rounded-xl border p-4 text-left transition-colors ${selectedProfile === 'custom' ? 'border-pink-500 bg-pink-50 dark:bg-pink-500/10' : 'border-zinc-200 bg-white hover:border-pink-300 dark:border-white/10 dark:bg-suno-card'} disabled:cursor-not-allowed`}>
            <div className="flex items-center gap-3"><span className={`grid h-5 w-5 place-items-center rounded-full border ${selectedProfile === 'custom' ? 'border-pink-500 bg-pink-500 text-white' : 'border-zinc-300 dark:border-zinc-600'}`}>{selectedProfile === 'custom' && <Check size={13} strokeWidth={3} />}</span><span className="min-w-0 flex-1 font-semibold text-zinc-900 dark:text-white">Custom compatible set</span><span className="shrink-0 text-xs text-zinc-500 dark:text-zinc-400">{bytes(selectedCustomBytes)}</span></div>
            <div className="ml-8 mt-1 text-xs text-zinc-500 dark:text-zinc-400">Exactly one of each of the five native component categories</div>
          </button>}
        </div>

        <button type="button" onClick={() => setAdvanced((value) => !value)} disabled={!!active} className="mt-4 text-xs font-medium text-zinc-500 hover:text-pink-500 disabled:opacity-50">
          {advanced ? 'Hide alternative complete profiles' : 'Show all runnable profiles and custom builder'}
        </button>

        {advanced && selectedProfile === 'custom' && <div className="mt-4 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-suno-card">
          <div className="text-sm font-semibold text-zinc-900 dark:text-white">Custom compatible set</div>
          <p className="mt-1 text-xs leading-5 text-zinc-500 dark:text-zinc-400">Only pinned minimaxmusic.cpp GGUF components are listed. Choose one item in every category; the backend performs final compatibility validation before it starts any download.</p>
          <div className="mt-4 grid gap-3 sm:grid-cols-2">
            {componentGroups.map((group) => <label key={group.kind} className="block text-xs font-medium text-zinc-700 dark:text-zinc-200">
              <span>{componentKindLabel(group.kind)}</span>
              <select value={customComponents[group.kind] || ''} onChange={(event) => setCustomComponents((current) => ({ ...current, [group.kind]: event.target.value }))} disabled={!!active} className="mt-1.5 w-full rounded-lg border border-zinc-200 bg-zinc-50 px-3 py-2 text-sm text-zinc-900 focus:border-pink-500 focus:outline-none disabled:opacity-50 dark:border-white/10 dark:bg-black/20 dark:text-white">
                <option value="">Choose a component</option>
                {group.components.map((component) => <option key={component.id} value={component.id}>{componentPrecision(component)} — {bytes(component.bytes)}</option>)}
              </select>
              <span className="mt-1 block truncate font-normal text-zinc-500 dark:text-zinc-400">{group.components.find((component) => component.id === customComponents[group.kind])?.filename || 'No component selected'}</span>
            </label>)}
          </div>
          <div className="mt-3 text-xs text-zinc-500 dark:text-zinc-400">{selectedCustomIds ? `Complete native set: 5 / 5 components · ${bytes(selectedCustomBytes)}` : 'Incomplete: choose all five categories before downloading.'}</div>
        </div>}

        {active?.status === 'downloading' && <div className="mt-5 rounded-xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-suno-card">
          <div className="flex items-center justify-between text-sm"><span className="flex items-center gap-2 font-medium text-zinc-700 dark:text-zinc-200"><Loader2 size={16} className="animate-spin text-pink-500" />Downloading {active.profile_id || 'custom compatible set'}</span><span className="text-zinc-500">{progress.toFixed(1)}%</span></div>
          <div className="mt-3 h-2 overflow-hidden rounded-full bg-zinc-200 dark:bg-black/30"><div className="h-full bg-gradient-to-r from-pink-500 to-purple-500 transition-[width]" style={{ width: `${progress}%` }} /></div>
          <div className="mt-2 text-xs text-zinc-500">{bytes(active.downloaded_bytes)} / {bytes(active.total_bytes)}</div>
        </div>}

        <div className="mt-6 flex gap-3">
          {active?.status === 'downloading' ? <button type="button" onClick={() => void cancel()} className="inline-flex items-center gap-2 rounded-xl border border-zinc-300 px-4 py-2.5 text-sm font-semibold text-zinc-700 hover:border-rose-400 hover:text-rose-600 dark:border-white/15 dark:text-zinc-200"><Square size={15} />Cancel download</button> : <button type="button" onClick={() => void download()} disabled={(!selected && !selectedCustomIds) || !!active} className="inline-flex items-center gap-2 rounded-xl bg-gradient-to-r from-orange-500 to-pink-600 px-5 py-2.5 text-sm font-bold text-white shadow-lg hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"><Download size={16} />Download {selectedProfile === 'custom' ? 'custom compatible set' : 'selected profile'}</button>}
        </div>
      </div>
    </div>
  );
};
