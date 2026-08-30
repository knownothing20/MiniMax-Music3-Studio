import {
  readOmniBridgeIntegrationStatus,
  type FetchLike,
  type OmniBridgeIntegrationStatus,
} from './omnibridgeMusic';

export const CLOUD_FIRST_RUN_STORAGE_KEY = 'music3.cloud-first-run.completed.v1';
export const GENERATION_MODE_STORAGE_KEY = 'music3.generation-mode.v1';

export type GenerationModePreference = 'auto' | 'cloud' | 'local' | 'device-local';
export type GenerationExecutionTarget = 'cloud' | 'local' | 'device-local';

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

export function isOmniBridgeLocalReady(
  status: Pick<OmniBridgeIntegrationStatus, 'configured' | 'contractVerified'> | null | undefined,
): boolean {
  return status?.configured === true && status.contractVerified === true;
}

export function parseGenerationModePreference(value: string | null | undefined): GenerationModePreference {
  return value === 'cloud' || value === 'local' || value === 'device-local' ? value : 'auto';
}

export function resolveGenerationExecutionTarget(
  preference: GenerationModePreference,
  cloudAvailable: boolean,
  localRouteAvailable: boolean,
  deviceLocalAvailable: boolean,
): GenerationExecutionTarget | null {
  if (preference === 'cloud') return cloudAvailable ? 'cloud' : null;
  if (preference === 'local') return localRouteAvailable ? 'local' : null;
  if (preference === 'device-local') return deviceLocalAvailable ? 'device-local' : null;
  if (cloudAvailable) return 'cloud';
  if (localRouteAvailable) return 'local';
  if (deviceLocalAvailable) return 'device-local';
  return null;
}

export function readStudioExecutionStatus(
  fetchImpl?: FetchLike,
): Promise<OmniBridgeIntegrationStatus> {
  return readOmniBridgeIntegrationStatus(fetchImpl);
}
