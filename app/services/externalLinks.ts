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

type Opener = { openUrl?: (url: string) => Promise<void> };

function opener(): Opener | null {
  const tauri = (window as unknown as { __TAURI__?: { opener?: Opener } }).__TAURI__;
  return tauri?.opener ?? null;
}

/** True inside the desktop shell. */
export function isDesktop(): boolean {
  return Boolean((window as unknown as { __TAURI__?: unknown }).__TAURI__);
}

/** Opens one URL wherever it belongs. */
export async function openExternal(url: string): Promise<void> {
  const open = opener()?.openUrl;
  if (open) {
    await open(url).catch(() => window.open(url, '_blank', 'noopener'));
    return;
  }
  window.open(url, '_blank', 'noopener');
}

/** Sends every external link click to the system browser. Call once. */
export function installExternalLinkHandler(): void {
  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey) return;
    const anchor = (event.target as HTMLElement | null)?.closest?.('a');
    const href = anchor?.getAttribute('href');
    if (!href || !/^https?:\/\//i.test(href)) return;
    event.preventDefault();
    void openExternal(href);
  });
}
