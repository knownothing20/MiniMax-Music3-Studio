/** @vitest-environment happy-dom */

import React from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../context/I18nContext';
import { MUSIC3_LYRICS_STRATEGY_STORAGE_KEY, SIMPLE_CAPTION_REWRITER_STORAGE_KEY } from '../services/assistantBrief';
import { CreatePanel } from './CreatePanel';

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

describe('CreatePanel Caption Rewriter control', () => {
  beforeEach(() => {
    localStorage.clear();
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/v1/assistant/status')) return json({ provider: 'local', available: true });
      if (url.endsWith('/setup/status')) return json({ ready: true, engine_ready: true });
      if (url.endsWith('/v1/local-models/music')) return json({ catalog: { defaults: {}, models: {} } });
      if (url.endsWith('/v1/activity')) return json({ activity: [] });
      return json({});
    }));
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    document.body.replaceChildren();
  });

  it('shares one compact, overflow-safe switch across Studio and simple mode', async () => {
    const host = document.createElement('div');
    document.body.appendChild(host);
    const root = createRoot(host);

    flushSync(() => {
      root.render(
        <I18nProvider>
          <CreatePanel
            onGenerate={vi.fn()}
            isGenerating={false}
            executionTarget="cloud"
            generationMode="auto"
            onGenerationModeChange={vi.fn()}
            cloudAvailable
            localRouteAvailable
            deviceLocalAvailable
          />
        </I18nProvider>,
      );
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    flushSync(() => undefined);

    const toolbar = host.querySelector<HTMLElement>('[data-testid="caption-rewriter-toolbar"]');
    expect(toolbar).not.toBeNull();
    expect(toolbar?.className).toContain('max-w-full');
    expect(toolbar?.className).toContain('overflow-hidden');
    expect(toolbar?.querySelector('[data-testid="caption-rewriter-copy"]')?.className).toContain('min-w-0');

    const strategyToolbar = host.querySelector<HTMLElement>('[data-testid="lyrics-strategy-toolbar"]');
    expect(strategyToolbar?.className).toContain('max-w-full');
    const storyButton = strategyToolbar?.querySelector<HTMLButtonElement>('[aria-pressed="true"]');
    expect(storyButton?.textContent).toContain('故事写歌');
    const standardButton = Array.from(strategyToolbar?.querySelectorAll('button') || [])
      .find(button => button.textContent?.includes('标准写词'));
    flushSync(() => standardButton?.click());
    expect(localStorage.getItem(MUSIC3_LYRICS_STRATEGY_STORAGE_KEY)).toBe('standard');

    let switchButton = toolbar?.querySelector<HTMLButtonElement>('[role="switch"]');
    expect(switchButton?.className).toContain('shrink-0');
    expect(switchButton?.getAttribute('aria-checked')).toBe('true');
    expect(host.querySelector('[data-testid="caption-rewriter-mode"]')?.textContent).toContain('Music3 Caption Rewriter');

    flushSync(() => switchButton?.click());
    expect(switchButton?.getAttribute('aria-checked')).toBe('false');
    expect(localStorage.getItem(SIMPLE_CAPTION_REWRITER_STORAGE_KEY)).toBe('false');
    expect(host.querySelector('[data-testid="caption-rewriter-mode"]')?.textContent).toContain('标准描述规则');

    const simpleMode = Array.from(host.querySelectorAll('button'))
      .find((button) => button.textContent?.includes(String.fromCharCode(0x7b80, 0x6613)));
    flushSync(() => simpleMode?.click());
    expect(host.querySelectorAll('[data-testid="caption-rewriter-toolbar"]')).toHaveLength(1);
    expect(host.querySelector('[data-testid="lyrics-strategy-toolbar"] [aria-pressed="true"]')?.textContent).toContain('标准写词');
    switchButton = host.querySelector('[data-testid="caption-rewriter-toolbar"] [role="switch"]');
    expect(switchButton?.getAttribute('aria-checked')).toBe('false');

    flushSync(() => switchButton?.click());
    expect(localStorage.getItem(SIMPLE_CAPTION_REWRITER_STORAGE_KEY)).toBe('true');

    const studioMode = Array.from(host.querySelectorAll('button'))
      .find((button) => button.textContent?.trim() === '工作室');
    flushSync(() => studioMode?.click());
    expect(host.querySelectorAll('[data-testid="caption-rewriter-toolbar"]')).toHaveLength(1);
    expect(host.querySelector('[data-testid="caption-rewriter-toolbar"] [role="switch"]')?.getAttribute('aria-checked')).toBe('true');
    expect(host.querySelector('[data-testid="caption-rewriter-status"]')).toBeNull();

    const fetchMock = vi.mocked(fetch);
    const callsBeforeStyle = fetchMock.mock.calls.length;
    const styleButton = host.querySelector<HTMLButtonElement>('[data-testid="caption-style-suggestions"] button');
    const style = styleButton?.textContent?.trim() || '';
    flushSync(() => styleButton?.click());
    expect(host.querySelector<HTMLTextAreaElement>('#caption-assistant-brief')?.value).toContain(style);
    expect(host.querySelector<HTMLTextAreaElement>('#lyrics-assistant-brief')?.value).toBe('');
    expect(fetchMock.mock.calls.slice(callsBeforeStyle).some(([input]) => String(input).includes('/v1/assistant/'))).toBe(false);

    flushSync(() => root.unmount());
    host.remove();
  });

  it('runs simple mode as isolated lyrics then caption stages and keeps stage one on caption failure', async () => {
    const assistantPayloads: Array<Record<string, unknown>> = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/v1/assistant/status')) return json({ provider: 'local', available: true });
      if (url.endsWith('/setup/status')) return json({ ready: true, engine_ready: true });
      if (url.endsWith('/v1/local-models/music')) return json({ catalog: { defaults: {}, models: {} } });
      if (url.endsWith('/v1/activity')) return json({ activity: [] });
      if (url.endsWith('/v1/assistant/write/stream')) {
        const payload = JSON.parse(String(init?.body)) as Record<string, unknown>;
        assistantPayloads.push(payload);
        if (payload.target === 'lyrics') {
          const frame = {
            stage: 'done',
            draft: { title: '灵魂的回声', lyrics: '[verse]\n灵魂本就孤独\n[chorus]\n同频的人看见光' },
            audit: { stage: 'lyrics', strategy_name: 'story_songwriting', contract_version: 'story_songwriting.v1', input_summary: {}, output_summary: {}, validation: [], compression_actions: [] },
          };
          return new Response('data: ' + JSON.stringify(frame) + '\n\n', { status: 200, headers: { 'Content-Type': 'text/event-stream' } });
        }
        return new Response(JSON.stringify({ error: 'caption stage unavailable' }), { status: 502, headers: { 'Content-Type': 'application/json' } });
      }
      return json({});
    }));

    const host = document.createElement('div');
    document.body.appendChild(host);
    const root = createRoot(host);
    flushSync(() => {
      root.render(
        <I18nProvider>
          <CreatePanel
            onGenerate={vi.fn()}
            isGenerating={false}
            executionTarget="cloud"
            generationMode="auto"
            onGenerationModeChange={vi.fn()}
            cloudAvailable
            localRouteAvailable
            deviceLocalAvailable
          />
        </I18nProvider>,
      );
    });
    await new Promise(resolve => setTimeout(resolve, 0));
    flushSync(() => undefined);

    const simpleMode = Array.from(host.querySelectorAll('button')).find(button => button.textContent?.includes('简易'));
    flushSync(() => simpleMode?.click());
    const idea = host.querySelectorAll<HTMLTextAreaElement>('textarea')[0];
    flushSync(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      setter?.call(idea, '把书评改成温柔克制的女声歌曲');
      idea.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const create = Array.from(host.querySelectorAll('button')).find(button => button.textContent?.includes('生成完整歌曲方案'));
    flushSync(() => create?.click());
    await new Promise(resolve => setTimeout(resolve, 20));
    flushSync(() => undefined);

    expect(assistantPayloads.map(payload => payload.target)).toEqual(['lyrics', 'prompt']);
    expect(assistantPayloads[0]).toMatchObject({ lyrics_strategy: 'story_songwriting', use_caption_rewriter: true });
    expect(assistantPayloads[1]).toMatchObject({ lyrics_strategy: 'story_songwriting', use_caption_rewriter: true });
    expect(String(assistantPayloads[1].lyrics)).toContain('灵魂本就孤独');
    expect(Array.from(host.querySelectorAll<HTMLTextAreaElement>('textarea')).some(area => area.value.includes('灵魂本就孤独'))).toBe(true);
    expect(host.querySelector('[data-testid="retry-simple-caption"]')).not.toBeNull();
    expect(Array.from(host.querySelectorAll<HTMLInputElement>('input')).some(input => input.value === '灵魂的回声')).toBe(true);

    flushSync(() => root.unmount());
    host.remove();
  });

  it('submits OmniBridge local without polling or requiring the device engine', async () => {
    const onGenerate = vi.fn();
    const host = document.createElement('div');
    document.body.appendChild(host);
    const root = createRoot(host);

    flushSync(() => {
      root.render(
        <I18nProvider>
          <CreatePanel
            onGenerate={onGenerate}
            isGenerating={false}
            executionTarget="local"
            generationMode="local"
            onGenerationModeChange={vi.fn()}
            cloudAvailable
            localRouteAvailable
            deviceLocalAvailable={false}
          />
        </I18nProvider>,
      );
    });
    await new Promise(resolve => setTimeout(resolve, 0));
    const exampleButton = host.querySelector<HTMLButtonElement>('[title="示例"]');
    expect(exampleButton).not.toBeNull();
    flushSync(() => exampleButton?.click());
    const textareas = Array.from(host.querySelectorAll<HTMLTextAreaElement>('textarea'));
    expect(textareas.some(area => area.value.trim().length > 0)).toBe(true);
    flushSync(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      textareas.forEach(area => {
        setter?.call(area, 'short test copy');
        area.dispatchEvent(new Event('input', { bubbles: true }));
      });
    });
    const createButton = Array.from(host.querySelectorAll('button')).find(button => button.textContent?.trim() === '创作');
    expect(createButton).not.toBeUndefined();
    flushSync(() => createButton?.click());

    expect(host.querySelector('[role="alert"]')?.textContent).toBeUndefined();
    expect(onGenerate).toHaveBeenCalledOnce();
    expect(onGenerate.mock.calls[0][0]).toMatchObject({ execution_target: 'local' });
    expect(onGenerate.mock.calls[0][0]).not.toHaveProperty('steps');
    const urls = vi.mocked(fetch).mock.calls.map(([input]) => String(input));
    expect(urls).not.toContain('/setup/status');
    expect(urls).not.toContain('/v1/local-models/music');

    flushSync(() => root.unmount());
    host.remove();
  });

  it('keeps setup polling and native engine controls for device-local', async () => {
    const onGenerate = vi.fn();
    const host = document.createElement('div');
    document.body.appendChild(host);
    const root = createRoot(host);

    flushSync(() => {
      root.render(
        <I18nProvider>
          <CreatePanel
            onGenerate={onGenerate}
            isGenerating={false}
            executionTarget="device-local"
            generationMode="device-local"
            onGenerationModeChange={vi.fn()}
            cloudAvailable
            localRouteAvailable
            deviceLocalAvailable
          />
        </I18nProvider>,
      );
    });
    await new Promise(resolve => setTimeout(resolve, 0));
    const exampleButton = host.querySelector<HTMLButtonElement>('[title="示例"]');
    expect(exampleButton).not.toBeNull();
    flushSync(() => exampleButton?.click());
    flushSync(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      host.querySelectorAll<HTMLTextAreaElement>('textarea').forEach(area => {
        setter?.call(area, 'short test copy');
        area.dispatchEvent(new Event('input', { bubbles: true }));
      });
    });
    const createButton = Array.from(host.querySelectorAll('button')).find(button => button.textContent?.trim() === '创作');
    flushSync(() => createButton?.click());

    expect(onGenerate).toHaveBeenCalledOnce();
    expect(onGenerate.mock.calls[0][0]).toMatchObject({ execution_target: 'device-local', steps: 30 });
    const urls = vi.mocked(fetch).mock.calls.map(([input]) => String(input));
    expect(urls).toContain('/setup/status');
    expect(urls).toContain('/v1/local-models/music');

    flushSync(() => root.unmount());
    host.remove();
  });

});
