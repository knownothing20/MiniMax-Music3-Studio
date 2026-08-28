import { describe, expect, it } from 'vitest';
import {
  isOmniBridgeCloudReady,
  parseGenerationModePreference,
  resolveGenerationExecutionTarget,
  type OmniBridgeReadinessEvidence,
} from './studioExecution';

const READY: OmniBridgeReadinessEvidence = {
  configured: true,
  executionTarget: 'omnibridge',
  contractVerified: true,
  routeResolutionVerified: true,
  providerResolutionVerified: true,
};

describe('isOmniBridgeCloudReady', () => {
  it('requires the complete cloud readiness evidence set', () => {
    expect(isOmniBridgeCloudReady(READY)).toBe(true);
  });

  it.each([
    ['configured', false],
    ['executionTarget', 'local'],
    ['contractVerified', false],
    ['routeResolutionVerified', false],
    ['providerResolutionVerified', false],
  ] as const)('rejects readiness when %s is not ready', (key, value) => {
    expect(isOmniBridgeCloudReady({ ...READY, [key]: value })).toBe(false);
  });

  it('fails closed when status is absent', () => {
    expect(isOmniBridgeCloudReady(null)).toBe(false);
    expect(isOmniBridgeCloudReady(undefined)).toBe(false);
  });
});

describe('generation mode preference', () => {
  it('defaults missing and invalid stored values to auto', () => {
    expect(parseGenerationModePreference(null)).toBe('auto');
    expect(parseGenerationModePreference('invalid')).toBe('auto');
    expect(parseGenerationModePreference('cloud')).toBe('cloud');
    expect(parseGenerationModePreference('local')).toBe('local');
  });

  it('prefers cloud in automatic mode and falls back to a ready local engine', () => {
    expect(resolveGenerationExecutionTarget('auto', true, true)).toBe('omnibridge');
    expect(resolveGenerationExecutionTarget('auto', false, true)).toBe('configuration');
    expect(resolveGenerationExecutionTarget('auto', false, false)).toBeNull();
  });

  it('never silently changes an explicit source', () => {
    expect(resolveGenerationExecutionTarget('cloud', true, true)).toBe('omnibridge');
    expect(resolveGenerationExecutionTarget('cloud', false, true)).toBeNull();
    expect(resolveGenerationExecutionTarget('local', true, true)).toBe('configuration');
    expect(resolveGenerationExecutionTarget('local', true, false)).toBeNull();
  });
});
