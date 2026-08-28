import { describe, expect, it } from 'vitest';
import { resolveInitialLanguage } from './I18nContext';

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
