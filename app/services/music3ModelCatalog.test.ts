import { describe, expect, it } from 'vitest';
import { completeCustomComponentIds, componentPrecision, componentsByKind, type Music3Component } from './music3ModelCatalog';

const components: Music3Component[] = [
  { id: 'lm-q5', kind: 'lm', filename: 'MiniMax-Music3-language_model-Q5_K_M.gguf', bytes: 1, sha256: 'a' },
  { id: 'depth-q8', kind: 'depth', filename: 'MiniMax-Music3-rvq_depth_decoder-Q8_0.gguf', bytes: 1, sha256: 'b' },
  { id: 'condition-f32', kind: 'condition', filename: 'MiniMax-Music3-condition_encoder-F32.gguf', bytes: 1, sha256: 'c' },
  { id: 'dit-q4', kind: 'dit', filename: 'MiniMax-Music3-transformer-Q4_K_M.gguf', bytes: 1, sha256: 'd' },
  { id: 'vocoder-f32', kind: 'vocoder', filename: 'MiniMax-Music3-vocoder-F32.gguf', bytes: 1, sha256: 'e' },
];

describe('Music3 model catalog helpers', () => {
  it('accepts exactly one known component for every runnable native category', () => {
    expect(completeCustomComponentIds(components, {
      lm: 'lm-q5', depth: 'depth-q8', condition: 'condition-f32', dit: 'dit-q4', vocoder: 'vocoder-f32',
    })).toEqual(['lm-q5', 'depth-q8', 'condition-f32', 'dit-q4', 'vocoder-f32']);
    expect(completeCustomComponentIds(components, { lm: 'lm-q5', depth: 'depth-q8' })).toBeNull();
    expect(componentsByKind(components).every((group) => group.components.length === 1)).toBe(true);
  });

  it('derives the displayed precision from the pinned native filename', () => {
    expect(componentPrecision(components[0])).toBe('Q5_K_M');
    expect(componentPrecision(components[2])).toBe('F32');
  });
});
