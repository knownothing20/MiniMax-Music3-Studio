import { describe, expect, it } from 'vitest';
import { buildAssistantInstruction } from './assistantBrief';

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
