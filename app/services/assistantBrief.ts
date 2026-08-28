export type AssistantTarget = 'all' | 'lyrics' | 'prompt';
export type LyricsLanguage = 'auto' | 'zh' | 'en';

type AssistantBriefs = {
  all: string;
  prompt: string;
  lyrics: string;
};

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
