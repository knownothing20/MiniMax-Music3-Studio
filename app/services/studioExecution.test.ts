import { describe, expect, it } from 'vitest';
import {
  isOmniBridgeCloudReady,
  isOmniBridgeLocalReady,
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

describe('isOmniBridgeLocalReady', () => {
  it('requires only the shared OmniBridge contract and never a device engine', () => {
    expect(isOmniBridgeLocalReady({ configured: true, contractVerified: true })).toBe(true);
    expect(isOmniBridgeLocalReady({ configured: true, contractVerified: false })).toBe(false);
    expect(isOmniBridgeLocalReady({ configured: false, contractVerified: true })).toBe(false);
  });
});

describe('generation mode preference', () => {
  it('defaults missing and invalid stored values to auto', () => {
    expect(parseGenerationModePreference(null)).toBe('auto');
    expect(parseGenerationModePreference('invalid')).toBe('auto');
    expect(parseGenerationModePreference('cloud')).toBe('cloud');
    expect(parseGenerationModePreference('local')).toBe('local');
    expect(parseGenerationModePreference('device-local')).toBe('device-local');
  });

  it('prefers cloud, then the OmniBridge local route, then a ready device engine', () => {
    expect(resolveGenerationExecutionTarget('auto', true, true, true)).toBe('cloud');
    expect(resolveGenerationExecutionTarget('auto', false, true, true)).toBe('local');
    expect(resolveGenerationExecutionTarget('auto', false, false, true)).toBe('device-local');
    expect(resolveGenerationExecutionTarget('auto', false, false, false)).toBeNull();
  });

  it('never silently changes an explicit source', () => {
    expect(resolveGenerationExecutionTarget('cloud', true, true, true)).toBe('cloud');
    expect(resolveGenerationExecutionTarget('cloud', false, true, true)).toBeNull();
    expect(resolveGenerationExecutionTarget('local', true, true, true)).toBe('local');
    expect(resolveGenerationExecutionTarget('local', true, false, true)).toBeNull();
    expect(resolveGenerationExecutionTarget('device-local', true, true, true)).toBe('device-local');
    expect(resolveGenerationExecutionTarget('device-local', true, true, false)).toBeNull();
  });
});
