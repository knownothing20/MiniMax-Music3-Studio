import React, { useEffect, useState } from 'react';
import { Boxes, RefreshCw, ShieldCheck } from 'lucide-react';
import { OpenRouterMediaPanel } from './OpenRouterMediaPanel';

interface SetupStatus {
  ready?: boolean;
  active_profile?: string;
  message?: string;
}

interface Capabilities {
  engines?: Array<{
    id?: string;
    label?: string;
    execution_mode?: string;
    capabilities?: string[];
  }>;
}

export function StudioToolsPanel(): React.ReactElement {
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = async () => {
    setLoading(true);
    try {
      const [setupResponse, capabilitiesResponse] = await Promise.all([
        fetch('/setup/status'),
        fetch('/v1/capabilities'),
      ]);
      setSetup(setupResponse.ok ? await setupResponse.json() : null);
      setCapabilities(capabilitiesResponse.ok ? await capabilitiesResponse.json() : null);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { void refresh(); }, []);

  const engines = capabilities?.engines ?? [];

  return (
    <div className="flex-1 overflow-y-auto bg-white dark:bg-suno px-5 py-6 md:px-8">
      <div className="mx-auto max-w-6xl space-y-6">
        <div className="flex items-start justify-between gap-4">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.18em] text-pink-500">Studio tools</p>
            <h1 className="mt-1 text-2xl font-bold text-zinc-950 dark:text-white">Runtimes, packages and compatibility</h1>
            <p className="mt-2 max-w-3xl text-sm text-zinc-600 dark:text-zinc-400">
              Native engine diagnostics, installed model packages and OpenRouter capabilities for this Music3 desktop runtime.
            </p>
          </div>
          <button onClick={() => void refresh()} className="inline-flex items-center gap-2 rounded-lg border border-zinc-200 px-3 py-2 text-sm text-zinc-700 hover:bg-zinc-50 dark:border-white/10 dark:text-zinc-200 dark:hover:bg-white/5">
            <RefreshCw size={15} className={loading ? 'animate-spin' : ''} /> Refresh
          </button>
        </div>

        <div className="grid gap-4 md:grid-cols-2">
          <section className="rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/[0.03]">
            <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white"><ShieldCheck size={17} className="text-pink-500" /> Native music runtime</div>
            <p className="mt-3 text-sm text-zinc-600 dark:text-zinc-400">
              {setup ? (setup.ready ? `Ready${setup.active_profile ? `: ${setup.active_profile}` : ''}` : (setup.message || 'Not installed yet. Open Create to install a complete compatible package.')) : 'Runtime status is unavailable.'}
            </p>
          </section>
          <section className="rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/[0.03]">
            <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white"><Boxes size={17} className="text-pink-500" /> Available provider capabilities</div>
            <p className="mt-3 text-sm text-zinc-600 dark:text-zinc-400">
              {engines.length ? engines.map((engine) => `${engine.label || engine.id}: ${(engine.capabilities || []).join(', ') || 'no capabilities reported'}`).join(' · ') : 'No provider capability catalog is currently reported by the native server.'}
            </p>
          </section>
        </div>

        <OpenRouterMediaPanel />
      </div>
    </div>
  );
}
