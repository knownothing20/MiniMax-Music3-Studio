import {
  readOmniBridgeIntegrationStatus,
  type FetchLike,
  type OmniBridgeIntegrationStatus,
} from './omnibridgeMusic';

export const CLOUD_FIRST_RUN_STORAGE_KEY = 'music3.cloud-first-run.completed.v1';
export const GENERATION_MODE_STORAGE_KEY = 'music3.generation-mode.v1';

export type GenerationModePreference = 'auto' | 'cloud' | 'local';
export type GenerationExecutionTarget = 'omnibridge' | 'configuration';

export type OmniBridgeReadinessEvidence = Pick<
  OmniBridgeIntegrationStatus,
  | 'configured'
  | 'executionTarget'
  | 'contractVerified'
  | 'routeResolutionVerified'
  | 'providerResolutionVerified'
>;

export function isOmniBridgeExecutionTarget(
  status: Pick<OmniBridgeIntegrationStatus, 'executionTarget'> | null | undefined,
): boolean {
  return status?.executionTarget === 'omnibridge';
}

export function isOmniBridgeCloudReady(
  status: OmniBridgeReadinessEvidence | null | undefined,
): boolean {
  return status?.configured === true
    && status.executionTarget === 'omnibridge'
    && status.contractVerified === true
    && status.routeResolutionVerified === true
    && status.providerResolutionVerified === true;
}

export function parseGenerationModePreference(value: string | null | undefined): GenerationModePreference {
  return value === 'cloud' || value === 'local' ? value : 'auto';
}

export function resolveGenerationExecutionTarget(
  preference: GenerationModePreference,
  cloudAvailable: boolean,
  localAvailable: boolean,
): GenerationExecutionTarget | null {
  if (preference === 'cloud') return cloudAvailable ? 'omnibridge' : null;
  if (preference === 'local') return localAvailable ? 'configuration' : null;
  if (cloudAvailable) return 'omnibridge';
  if (localAvailable) return 'configuration';
  return null;
}

export function readStudioExecutionStatus(
  fetchImpl?: FetchLike,
): Promise<OmniBridgeIntegrationStatus> {
  return readOmniBridgeIntegrationStatus(fetchImpl);
}
