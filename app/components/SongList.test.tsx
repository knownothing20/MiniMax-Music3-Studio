/** @vitest-environment happy-dom */

import React from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AuthProvider } from '../context/AuthContext';
import { I18nProvider } from '../context/I18nContext';
import type { Song } from '../types';
import { SongList } from './SongList';

function mount(node: React.ReactNode) {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const root = createRoot(host);
  flushSync(() => root.render(node));
  return { host, unmount: () => flushSync(() => { root.unmount(); host.remove(); }) };
}

const unknownSong: Song = {
  id: 'recovered-omnibridge-temp_123',
  jobId: 'omnibridge-temp_123',
  title: '中国人会飞',
  lyrics: '',
  style: 'unknown submit',
  coverUrl: '',
  duration: '--:--',
  createdAt: new Date('2026-08-30T00:00:00Z'),
  tags: ['music3'],
  isGenerating: false,
  submissionUnknown: true,
};

describe('unknown submission recovery card', () => {
  beforeEach(() => {
    localStorage.setItem('language', 'zh');
    vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })));
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    document.body.replaceChildren();
  });

  it('shows truthful unknown copy without progress or cancel controls', () => {
    const onDelete = vi.fn();
    const onRecoverUnknown = vi.fn();
    const onCancelJob = vi.fn();
    const view = mount(
      <AuthProvider>
        <I18nProvider>
          <SongList
            songs={[unknownSong]}
            currentSong={null}
            selectedSong={null}
            likedSongIds={new Set()}
            isPlaying={false}
            onPlay={vi.fn()}
            onSelect={vi.fn()}
            onToggleLike={vi.fn()}
            onAddToPlaylist={vi.fn()}
            onDelete={onDelete}
            onRecoverUnknown={onRecoverUnknown}
            onCancelJob={onCancelJob}
            activeJobCount={0}
          />
        </I18nProvider>
      </AuthProvider>,
    );

    expect(view.host.textContent).toContain('提交状态未知');
    expect(view.host.textContent).toContain('不能自动重试');
    expect(view.host.querySelector('[data-testid="generation-progress"]')).toBeNull();
    expect(view.host.textContent).not.toContain('取消生成');
    const buttons = Array.from(view.host.querySelectorAll('button'));
    const dismiss = buttons.find(button => button.textContent?.includes('从列表移除'));
    dismiss?.click();
    expect(onDelete).toHaveBeenCalledTimes(1);
    expect(onCancelJob).not.toHaveBeenCalled();
    view.unmount();
  });
});
