import { describe, expect, it } from 'vitest';
import { resolveApiBase } from './apiBase';

describe('resolveApiBase', () => {
  it('uses same-origin requests in browser development so Vite owns proxy routing', () => {
    expect(resolveApiBase({ development: true, tauri: false, locationPort: '3000' })).toBe('');
  });

  it('keeps an explicit API base as the highest-priority setting', () => {
    expect(resolveApiBase({
      override: 'http://127.0.0.1:18765/',
      development: true,
      tauri: false,
      locationPort: '3000',
    })).toBe('http://127.0.0.1:18765');
  });

  it('keeps Tauri and standalone production on the desktop loopback service', () => {
    expect(resolveApiBase({ development: true, tauri: true, locationPort: '3000' })).toBe('http://127.0.0.1:8765');
    expect(resolveApiBase({ development: false, tauri: false, locationPort: '4173' })).toBe('http://127.0.0.1:8765');
  });

  it('keeps a production page served by the service itself same-origin', () => {
    expect(resolveApiBase({ development: false, tauri: false, locationPort: '8765' })).toBe('');
  });
});
