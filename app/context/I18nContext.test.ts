import { describe, expect, it } from 'vitest';
import { resolveInitialLanguage } from './I18nContext';
import { translations } from '../i18n/translations';

describe('resolveInitialLanguage', () => {
  it('defaults a new studio to Simplified Chinese', () => {
    expect(resolveInitialLanguage(null)).toBe('zh');
  });

  it('respects an explicit English preference', () => {
    expect(resolveInitialLanguage('en')).toBe('en');
  });

  it.each(['zh', 'ja', 'ko', 'ru'] as const)('preserves %s', (language) => {
    expect(resolveInitialLanguage(language)).toBe(language);
  });

  it('falls back to Chinese for an invalid preference', () => {
    expect(resolveInitialLanguage('invalid')).toBe('zh');
  });
});

describe('assistant discovery copy', () => {
  it('keeps the Caption Rewriter status and both assistant scopes available in every language', () => {
    for (const copy of Object.values(translations)) {
      expect(copy.captionRewriterEnabled).toBeTruthy();
      expect(copy.captionRewriterIntegrated).toBeTruthy();
      expect(copy.structuredAssistantTitle).toBeTruthy();
      expect(copy.lyricsAssistantTitle).toBeTruthy();
      expect(copy.assistantCaptionScope).toBeTruthy();
      expect(copy.assistantLyricsScope).toBeTruthy();
      expect(copy.simpleCaptionRewriterLabel).toBeTruthy();
      expect(copy.simpleCaptionRewriterOnHint).toBeTruthy();
      expect(copy.simpleCaptionRewriterOffHint).toBeTruthy();
      expect(copy.simpleAssistantCompositionHint).toBeTruthy();
    }
    expect(translations.zh.captionRewriterEnabled).toBe('已启用');
    expect(translations.zh.simpleCaptionRewriterLabel).toBe('使用 Music3 Caption Rewriter');
    expect(translations.zh.simpleCaptionRewriterOffHint).toContain('歌词、标题和其他能力继续生成');
    expect(translations.zh.assistantCaptionScope).not.toBe(translations.zh.assistantLyricsScope);
  });
});
