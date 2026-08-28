/**
 * Links that leave the studio.
 *
 * The desktop window is a webview, not a browser: an anchor with
 * `target="_blank"` either does nothing or, worse, replaces the application
 * with a web page. Every external link is therefore handed to the system
 * browser through Tauri's opener, and in a plain browser it behaves as it
 * always did.
 *
 * One listener on the document covers every link in the interface, including
 * the ones inside news items, so nothing has to remember to be special.
 */

import { invoke, isTauri } from '@tauri-apps/api/core';

/** True inside the desktop shell. */
export function isDesktop(): boolean {
  return isTauri();
}

/** Opens one URL wherever it belongs. */
export async function openExternal(url: string): Promise<void> {
  if (isTauri()) {
    await invoke('plugin:opener|open_url', { url });
    return;
  }
  fallback(url);
}

/** Protected/blob media cannot be handed to another browser context. */
export function canOpenExternalAudioSource(source: string): boolean {
  try {
    const media = new URL(source, window.location.href);
    return media.protocol !== 'blob:'
      && !['/v1/', '/setup/', '/engine/'].some(prefix => media.pathname.startsWith(prefix));
  } catch {
    return false;
  }
}

/**
 * Opens the bundled editor only when its input can be fetched without the
 * private Studio session header. A system browser cannot inherit the Tauri
 * webview's in-memory token, and putting that token in this URL is forbidden.
 */
export async function openExternalAudioEditor(url: string): Promise<void> {
  try {
    const editor = new URL(url, window.location.href);
    const source = editor.searchParams.get('audioUrl');
    if (!source) throw new Error('missing audio source');
    const media = new URL(source, editor.origin);
    if (!canOpenExternalAudioSource(media.href)) {
      window.alert('Protected Studio media is not available in the external audio editor yet.');
      return;
    }
  } catch {
    window.alert('This audio source cannot be opened safely in the external editor.');
    return;
  }
  await openExternal(url);
}

function fallback(url: string): void {
  window.open(url, '_blank', 'noopener');
}

/** Sends every external link click to the system browser. Call once. */
export function installExternalLinkHandler(): void {
  // Capture, not bubble: dialogs stop propagation on their own container to
  // keep a click inside from closing them, and that also hid every link in
  // Settings and the news panel from a listener on the document.
  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey) return;
    const anchor = (event.target as HTMLElement | null)?.closest?.('a');
    const href = anchor?.getAttribute('href');
    if (!href || !/^https?:\/\//i.test(href)) return;
    event.preventDefault();
    void openExternal(href);
  }, true);
}
