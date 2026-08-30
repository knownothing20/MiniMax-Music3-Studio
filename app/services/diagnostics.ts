import type { Song } from '../types';

export function diagnosticSubjectId(song: Pick<Song, 'id' | 'jobId'>): string {
  return song.jobId?.trim() || song.id;
}

export function diagnosticDownloadPath(song: Pick<Song, 'id' | 'jobId'>): string {
  return `/v1/library/songs/${encodeURIComponent(diagnosticSubjectId(song))}/diagnostics`;
}

function safeFilename(header: string | null, subject: string): string {
  const declared = header?.match(/filename="?([^";]+)"?/i)?.[1];
  const leaf = declared?.split(/[\\/]/).pop()?.replace(/[\r\n"<>:*?|]/g, '').trim();
  if (leaf?.toLowerCase().endsWith('.zip')) return leaf;
  const safeSubject = subject.replace(/[^a-zA-Z0-9_-]/g, '') || 'song';
  return `music-maker-diagnostics-${safeSubject}.zip`;
}

export async function downloadSongDiagnostics(song: Pick<Song, 'id' | 'jobId'>): Promise<void> {
  const subject = diagnosticSubjectId(song);
  const response = await fetch(diagnosticDownloadPath(song));
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new Error(body?.error || `Diagnostic export failed (${response.status})`);
  }
  const blob = await response.blob();
  if (blob.size === 0) throw new Error('Diagnostic export was empty');
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = safeFilename(response.headers.get('content-disposition'), subject);
  document.body.appendChild(link);
  try {
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
}
