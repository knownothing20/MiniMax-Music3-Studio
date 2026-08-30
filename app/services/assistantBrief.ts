import type { LyricsStrategy } from './writingAssistant';

export type AssistantTarget = 'all' | 'lyrics' | 'prompt';
export type LyricsLanguage = 'auto' | 'zh' | 'en';

export const SIMPLE_CAPTION_REWRITER_STORAGE_KEY = 'music3-simple-caption-rewriter-enabled';
export const MUSIC3_LYRICS_STRATEGY_STORAGE_KEY = 'music3-lyrics-strategy';

type AssistantBriefs = {
  all: string;
  prompt: string;
  lyrics: string;
};

/** Story + Caption is the recommended new-user combination. */
export function resolveLyricsStrategyPreference(stored: string | null): LyricsStrategy {
  return stored === 'standard' ? 'standard' : 'story_songwriting';
}

/** A new or unreadable preference keeps the safer Music3-specific contract on. */
export function resolveCaptionRewriterPreference(stored: string | null): boolean {
  return stored !== 'false';
}

/**
 * Simple and Studio share one persisted Caption Rewriter choice. Turning it off
 * keeps structured description available through the standard project rules.
 */
export function useCaptionRewriterForTarget(
  _target: AssistantTarget,
  captionRewriterEnabled: boolean,
): boolean {
  return captionRewriterEnabled;
}

export function appendStyleSuggestion(brief: string, style: string): string {
  const cleanStyle = style.trim();
  if (!cleanStyle) return brief;
  const cleanBrief = brief.trim();
  if (!cleanBrief) return cleanStyle;
  if (cleanBrief.toLocaleLowerCase().includes(cleanStyle.toLocaleLowerCase())) return brief;
  return cleanBrief.replace(/[\s,;]+$/, '') + ', ' + cleanStyle;
}

const LANGUAGE_DIRECTIVES: Record<Exclude<LyricsLanguage, 'auto'>, string> = {
  zh: '歌词必须使用简体中文。',
  en: 'Lyrics must be written in English.',
};

/**
 * Keep the three assistant jobs independent. A blank task brief always stays
 * blank so callers can fail closed instead of asking the model to improvise
 * from hidden defaults.
 */
export function buildAssistantInstruction(
  target: AssistantTarget,
  briefs: AssistantBriefs,
  lyricsLanguage: LyricsLanguage,
  interfaceLanguage: string,
): string {
  const brief = briefs[target].trim();
  if (!brief) return '';
  if (target === 'prompt') return brief;

  const languageDirective = lyricsLanguage === 'auto'
    ? interfaceLanguage === 'zh'
      ? '歌词语言跟随用户要求；用户未指定时使用简体中文。'
      : 'Follow the lyric language requested by the user; otherwise use the language of the brief.'
    : LANGUAGE_DIRECTIVES[lyricsLanguage];
  return `${languageDirective}\n\n${brief}`;
}
