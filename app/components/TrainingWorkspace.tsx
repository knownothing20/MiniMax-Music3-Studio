import React from 'react';
import { FlaskConical, ShieldAlert } from 'lucide-react';
import { TrainingPanel } from './TrainingPanel';

export function TrainingWorkspace(): React.ReactElement {
  return (
    <div className="flex-1 overflow-y-auto bg-white dark:bg-suno-DEFAULT px-5 py-6 md:px-8">
      <div className="mx-auto max-w-6xl space-y-6">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.18em] text-pink-500">Adapters lab</p>
          <h1 className="mt-1 flex items-center gap-2 text-2xl font-bold text-zinc-950 dark:text-white"><FlaskConical size={24} /> Dataset and adapter workspace</h1>
          <p className="mt-2 max-w-3xl text-sm text-zinc-600 dark:text-zinc-400">
            The full ACE dataset, preprocessing and adapter workflow is retained for research and compatible runtimes. It is not represented as a production MiniMax adapter trainer until a reproducible training runtime and compatible adapter format are installed.
          </p>
        </div>

        <section className="rounded-2xl border border-amber-500/30 bg-amber-500/5 p-4 text-sm text-zinc-700 dark:text-zinc-300">
          <div className="flex items-start gap-2"><ShieldAlert size={18} className="mt-0.5 shrink-0 text-amber-500" /><p><strong className="text-zinc-950 dark:text-white">Experimental compatibility boundary.</strong> Dataset preparation remains useful now; checkpoint initialization, preprocessing and training commands only run when their selected backend is actually installed. No resulting ACE LoRA is offered to the native MiniMax inference engine as if it were compatible.</p></div>
        </section>

        <TrainingPanel />
      </div>
    </div>
  );
}
