import { describe, expect, it } from 'vitest';
import {
  appendStyleSuggestion,
  buildAssistantInstruction,
  resolveCaptionRewriterPreference,
  resolveLyricsStrategyPreference,
  useCaptionRewriterForTarget,
} from './assistantBrief';

const BRIEFS = {
  all: '写一首夜归主题的完整歌曲',
  prompt: '突出温暖合成器和渐进编曲',
  lyrics: '写城市夜雨中的重逢',
};

describe('buildAssistantInstruction', () => {
  it('keeps each assistant job on its own explicit brief', () => {
    expect(buildAssistantInstruction('prompt', BRIEFS, 'zh', 'zh')).toBe(BRIEFS.prompt);
    expect(buildAssistantInstruction('lyrics', BRIEFS, 'en', 'zh')).toContain(BRIEFS.lyrics);
    expect(buildAssistantInstruction('all', BRIEFS, 'zh', 'zh')).toContain(BRIEFS.all);
  });

  it('fails closed when the selected job has no brief', () => {
    expect(buildAssistantInstruction('lyrics', { ...BRIEFS, lyrics: '   ' }, 'auto', 'zh')).toBe('');
  });

  it('adds an explicit lyric language policy only to jobs that write lyrics', () => {
    expect(buildAssistantInstruction('lyrics', BRIEFS, 'zh', 'zh')).toMatch(/^歌词必须使用简体中文。/);
    expect(buildAssistantInstruction('all', BRIEFS, 'en', 'zh')).toMatch(/^Lyrics must be written in English\./);
    expect(buildAssistantInstruction('prompt', BRIEFS, 'zh', 'zh')).not.toContain('歌词');
  });

  it('defaults automatic lyric language to Chinese in the Chinese interface', () => {
    expect(buildAssistantInstruction('lyrics', BRIEFS, 'auto', 'zh')).toMatch(/未指定时使用简体中文/);
  });
});

describe('Caption style suggestions', () => {
  it('fills or appends the description brief without duplicating a selected style', () => {
    expect(appendStyleSuggestion('', 'Synthwave')).toBe('Synthwave');
    expect(appendStyleSuggestion('Warm female vocal', 'Synthwave')).toBe('Warm female vocal, Synthwave');
    expect(appendStyleSuggestion('Warm Synthwave', 'synthwave')).toBe('Warm Synthwave');
  });
});

describe('Lyrics strategy policy', () => {
  it('defaults to the recommended story method and preserves an explicit standard choice', () => {
    expect(resolveLyricsStrategyPreference(null)).toBe('story_songwriting');
    expect(resolveLyricsStrategyPreference('invalid')).toBe('story_songwriting');
    expect(resolveLyricsStrategyPreference('standard')).toBe('standard');
  });
});

describe('Caption Rewriter mode policy', () => {
  it('defaults the simple-mode preference to enabled', () => {
    expect(resolveCaptionRewriterPreference(null)).toBe(true);
    expect(resolveCaptionRewriterPreference('invalid')).toBe(true);
    expect(resolveCaptionRewriterPreference('false')).toBe(false);
  });

  it('shares one choice across simple, Studio description, and lyrics requests', () => {
    for (const target of ['all', 'prompt', 'lyrics'] as const) {
      expect(useCaptionRewriterForTarget(target, true)).toBe(true);
      expect(useCaptionRewriterForTarget(target, false)).toBe(false);
    }
  });
});
