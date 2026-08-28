/**
 * Where the studio service lives.
 *
 * In the packaged desktop application the UI is served from Tauri's asset
 * protocol, so a relative `/setup/status` never reaches the service — it
 * resolves against the asset host and comes back as the HTML shell, which the
 * caller then fails to parse as JSON. Development is different again: the Vite
 * server proxies those paths.
 *
 * One rule covers both: address the service directly on loopback and attach
 * the random desktop-session credential in memory. It is never put in a URL,
 * localStorage, sessionStorage or a log line.
 */

import { invoke, isTauri } from '@tauri-apps/api/core';

const DEFAULT_BASE = 'http://127.0.0.1:8765';

/** Paths owned by the studio service; everything else is left untouched. */
const SERVICE_PREFIXES = ['/v1/', '/setup/', '/engine/', '/health'];
const PROTECTED_PREFIXES = ['/v1/', '/setup/', '/engine/'];

export interface ApiBaseEnvironment {
  override?: string;
  development: boolean;
  tauri: boolean;
  locationPort?: string;
}

/** Pure environment decision used by the runtime and by contract tests. */
export function resolveApiBase(environment: ApiBaseEnvironment): string {
  const override = environment.override?.trim();
  if (override) return override.replace(/\/$/, '');
  // Tauri cannot rely on a Vite proxy, including during desktop development.
  if (environment.tauri) return DEFAULT_BASE;
  // Browser development is served by Vite; same-origin requests must use its
  // proxy so MUSIC_MAKER_DEV_API_TARGET can select the Rust test port.
  if (environment.development) return '';
  // Already served by the service itself: keep requests same-origin.
  if (environment.locationPort === '8765') return '';
  return DEFAULT_BASE;
}

const viteEnv = (import.meta as { env?: Record<string, string | boolean | undefined> }).env;
export const API_BASE = resolveApiBase({
  override: typeof viteEnv?.VITE_STUDIO_API_BASE === 'string' ? viteEnv.VITE_STUDIO_API_BASE : undefined,
  development: viteEnv?.DEV === true,
  tauri: isTauri(),
  locationPort: typeof location === 'undefined' ? undefined : location.port,
});
const SESSION_HEADER = 'X-Studio-Session';

async function loadSessionToken(): Promise<string> {
  const value = isTauri()
    ? await invoke<string>('studio_session_token')
    : (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_STUDIO_SESSION_TOKEN;
  if (typeof value !== 'string' || value.trim().length < 32) {
    throw new Error('The Studio session credential is unavailable.');
  }
  return value;
}

let sessionTokenPromise: Promise<string> | null = null;
function sessionToken(): Promise<string> {
  sessionTokenPromise ??= loadSessionToken();
  return sessionTokenPromise;
}

/** Fetch protected media with the in-memory session header and expose only a
 * browser-local Blob URL to audio/image elements, which cannot set headers. */
export async function authenticatedObjectUrl(path: string): Promise<string> {
  const response = await window.fetch(path);
  if (!response.ok) {
    throw new Error(`Studio media request failed (${response.status})`);
  }
  const blob = await response.blob();
  if (blob.size === 0) throw new Error('Studio media response was empty.');
  return URL.createObjectURL(blob);
}

export function apiUrl(path: string): string {
  if (!API_BASE || !path.startsWith('/')) return path;
  return `${API_BASE}${path}`;
}

function isServicePath(path: string): boolean {
  return SERVICE_PREFIXES.some((prefix) => path === prefix || path.startsWith(prefix));
}

function isProtectedPath(path: string): boolean {
  return PROTECTED_PREFIXES.some((prefix) => path === prefix || path.startsWith(prefix));
}

function servicePathFromUrl(value: string): string | null {
  try {
    const url = new URL(value, location.href);
    const serviceOrigin = new URL(API_BASE || location.origin, location.href).origin;
    if (url.origin !== serviceOrigin && url.origin !== location.origin) return null;
    const path = `${url.pathname}${url.search}`;
    return isServicePath(path) ? path : null;
  } catch {
    return null;
  }
}

/**
 * Rewrites service-owned relative requests once, centrally, so every caller can
 * keep using readable paths and no screen can be left behind pointing at the
 * wrong host.
 */
export function installApiBase(): void {
  if (typeof window === 'undefined') return;
  const original = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    let path: string | null = null;
    let target: RequestInfo | URL = input;
    if (typeof input === 'string' && input.startsWith('/') && isServicePath(input)) {
      path = input;
      target = apiUrl(input);
    } else if (input instanceof Request) {
      path = servicePathFromUrl(input.url);
      if (path) {
        target = new Request(apiUrl(path), input);
      }
    } else if (input instanceof URL) {
      path = servicePathFromUrl(input.href);
      if (path) target = apiUrl(path);
    } else if (typeof input === 'string') {
      path = servicePathFromUrl(input);
      if (path) target = apiUrl(path);
    }
    if (!path || !isProtectedPath(path)) return original(target, init);
    const headers = new Headers(target instanceof Request ? target.headers : init?.headers);
    headers.set(SESSION_HEADER, await sessionToken());
    if (target instanceof Request) return original(new Request(target, { ...init, headers }));
    return original(target, { ...init, headers });
  };
}
