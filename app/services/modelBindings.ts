export interface ModelSelector {
  type: 'inherit' | 'route';
  id?: string;
}

export interface CapabilityBinding {
  selector: ModelSelector;
  operation: string;
  required_capabilities: string[];
  mode: string;
  revision_policy?: string;
}

export interface RoleBinding {
  capability: 'text' | 'music';
  selector: ModelSelector;
}

export interface ProjectProfile {
  schema: 'omnibridge.project-profile.v2';
  project_id: 'music-maker';
  profile_revision: number;
  capability_defaults: Record<'text' | 'music', CapabilityBinding>;
  roles: Record<string, RoleBinding>;
}

export interface RoleDefinition {
  id: string;
  label_zh: string;
  description_zh: string;
  capability: 'text' | 'music';
}

export interface ProviderStrategy {
  route_id: string;
  display_name_zh: string;
  capability_family: string;
  tier?: string;
  description_zh?: string;
  fallback_enabled?: boolean;
  revision?: string;
  candidates?: Array<{ provider?: string; upstream_model?: string; ready?: boolean }>;
}

export interface ModelBindingsResponse {
  schema: 'music-maker.model-bindings.v1';
  profile: ProjectProfile;
  roles: RoleDefinition[];
  strategies: ProviderStrategy[];
  strategy_schema: string | null;
  hub: { available: boolean; centrally_managed: true; error?: string };
}

async function bodyOrError(response: Response): Promise<any> {
  const body = await response.json().catch(() => null);
  if (!response.ok) throw new Error(body?.error || body?.message || `请求失败（${response.status}）`);
  return body;
}

export async function readModelBindings(): Promise<ModelBindingsResponse> {
  return bodyOrError(await fetch('/v1/model-bindings'));
}

export async function previewModelBindings(profile: ProjectProfile): Promise<unknown> {
  return bodyOrError(await fetch('/v1/model-bindings/preview', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ profile }),
  }));
}

export async function saveModelBindings(profile: ProjectProfile, expectedRevision: number): Promise<ProjectProfile> {
  const result = await bodyOrError(await fetch('/v1/model-bindings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ expected_profile_revision: expectedRevision, profile }),
  }));
  return result.profile as ProjectProfile;
}
