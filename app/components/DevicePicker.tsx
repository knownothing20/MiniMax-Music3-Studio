import React from 'react';
import { useI18n } from '../context/I18nContext';

/**
 * What a local model runs on.
 *
 * One control, used everywhere the question is asked - the download page and
 * the tools panel had grown two versions of it that looked different and
 * behaved the same, which is how an interface starts teaching people that the
 * same thing in two places might mean two things.
 */
export type Device = 'auto' | 'cuda' | 'cpu';

export const DEVICES: Device[] = ['auto', 'cuda', 'cpu'];

export const DevicePicker: React.FC<{
  value: Device;
  onChange: (value: Device) => void;
  /** The card is not offered when its runtime is not installed. */
  cudaAvailable?: boolean;
}> = ({ value, onChange, cudaAvailable = true }) => {
  const { t } = useI18n();
  return (
    <div className="flex gap-1.5">
      {DEVICES.map(device => (
        <button
          key={device}
          type="button"
          onClick={() => onChange(device)}
          disabled={device === 'cuda' && !cudaAvailable}
          className={`flex-1 rounded-lg border px-3 py-2 text-xs font-semibold transition-colors disabled:opacity-40 ${
            value === device
              ? 'border-pink-400 bg-pink-500/10 text-zinc-900 dark:text-white'
              : 'border-zinc-200 text-zinc-500 hover:text-zinc-900 dark:border-white/10 dark:hover:text-white'
          }`}
        >
          {t(device === 'auto' ? 'separationRuntimeAuto' : device === 'cuda' ? 'separationRuntimeGpu' : 'separationRuntimeCpu')}
        </button>
      ))}
    </div>
  );
};
