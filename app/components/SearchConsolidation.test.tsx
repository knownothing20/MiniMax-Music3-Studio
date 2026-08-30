/** @vitest-environment happy-dom */

import React from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AuthProvider } from '../context/AuthContext';
import { I18nProvider } from '../context/I18nContext';
import type { Song } from '../types';
import { LibraryView } from './LibraryView';
import { Sidebar } from './Sidebar';

const songs: Song[] = [
  {
    id: 'night',
    title: 'Night Train',
    style: 'Global Metadata: synthwave',
    lyrics: '[chorus]\nBack to the city',
    tags: ['dreamy'],
    duration: '3:20',
    createdAt: new Date('2026-01-01'),
    coverUrl: '',
    audioUrl: '/night.wav',
    isPublic: false,
  },
  {
    id: 'day',
    title: 'Daylight',
    style: 'Acoustic folk',
    lyrics: '[chorus]\nMorning',
    tags: ['bright'],
    duration: '2:50',
    createdAt: new Date('2026-01-02'),
    coverUrl: '',
    audioUrl: '/day.wav',
    isPublic: false,
  },
];

function mount(node: React.ReactNode) {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const root = createRoot(host);
  flushSync(() => root.render(node));
  return { host, unmount: () => flushSync(() => { root.unmount(); host.remove(); }) };
}

describe('search consolidation UI', () => {
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

  it('does not render an independent Search item in the sidebar', () => {
    const view = mount(
      <I18nProvider>
        <Sidebar
          currentView="library"
          onNavigate={vi.fn()}
          theme="dark"
          onToggleTheme={vi.fn()}
          user={{ username: 'Local Studio' }}
          isOpen
        />
      </I18nProvider>,
    );
    const nav = view.host.querySelector('nav');
    expect(nav?.textContent).not.toContain('搜索');
    expect(Array.from(nav?.querySelectorAll('button') || []).some(button => button.title === '搜索')).toBe(false);
    view.unmount();
  });

  it('opens the library with a preserved query and filters the visible songs', () => {
    const view = mount(
      <AuthProvider>
        <I18nProvider>
          <LibraryView
            allSongs={songs}
            likedSongs={[]}
            playlists={[]}
            initialSearchQuery="dreamy"
            onPlaySong={vi.fn()}
            onCreatePlaylist={vi.fn()}
            onSelectPlaylist={vi.fn()}
            onAddToPlaylist={vi.fn()}
          />
        </I18nProvider>
      </AuthProvider>,
    );
    const input = view.host.querySelector<HTMLInputElement>('[data-testid="library-search"] input');
    expect(input?.value).toBe('dreamy');
    expect(view.host.textContent).toContain('Night Train');
    expect(view.host.textContent).not.toContain('Daylight');
    view.unmount();
  });
});
