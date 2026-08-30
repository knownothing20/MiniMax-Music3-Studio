export type WritingAssistantTarget = 'all' | 'lyrics' | 'prompt';
export type LyricsStrategy = 'standard' | 'story_songwriting';

export type WritingAssistantDraft = {
  lyrics?: string;
  global_metadata?: string;
  vocal_details?: string;
  arrangement?: string;
  title?: string;
  cover_prompt?: string;
  duration_seconds?: number;
};

export type WritingAssistantReceipt = {
  request_id?: string;
  route_id?: string;
  route_revision?: string;
  resolved_provider?: string;
  resolved_deployment?: string;
  resolved_model?: string;
  provider_adapter?: string;
  attempt?: number;
  outcome?: string;
};

export type WritingAssistantStatus = {
  provider?: string | null;
  available?: boolean;
  cloud_ready?: boolean;
  role?: string | null;
  role_id?: string | null;
  route?: string | null;
  route_id?: string | null;
  resolved_model?: string | null;
};

export type WritingAssistantRequest = {
  target: WritingAssistantTarget;
  description: string;
  instruction: string;
  lyrics: string;
  global_metadata: string;
  vocal_details: string;
  arrangement: string;
  duration_seconds: number;
  instrumental: boolean;
  /** Both modes share this independent description contract choice. */
  use_caption_rewriter: boolean;
  lyrics_strategy: LyricsStrategy;
};

export type WritingAssistantAudit = {
  stage: 'lyrics' | 'structured_caption' | 'legacy_combined';
  strategy_name: string;
  contract_version: string;
  input_summary: Record<string, unknown>;
  output_summary: Record<string, unknown>;
  validation: string[];
  compression_actions: string[];
};

export type WritingAssistantDone = {
  draft: WritingAssistantDraft;
  audit?: WritingAssistantAudit;
  text?: string;
  receipt?: WritingAssistantReceipt;
};

export type WritingAssistantStreamEvent = {
  stage?: string;
  delta?: string;
  text?: string;
  error?: string;
  model?: string;
  draft?: WritingAssistantDraft;
  audit?: WritingAssistantAudit;
  receipt?: WritingAssistantReceipt;
};

export type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

type StreamOptions = {
  fetchImpl?: FetchLike;
  signal?: AbortSignal;
  onEvent?: (event: WritingAssistantStreamEvent) => void;
};

export function isWritingAssistantAvailable(
  status: WritingAssistantStatus | null | undefined,
): boolean {
  if (!status) return false;
  if (status.provider === 'omnibridge') {
    return status.cloud_ready === true || status.available === true;
  }
  return status.available === true;
}

function isDraft(value: unknown): value is WritingAssistantDraft {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function parseFrame(frame: string): WritingAssistantStreamEvent | null {
  const data = frame
    .split(/\r?\n/)
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).trimStart())
    .join('\n');
  if (!data || data === '[DONE]') return null;
  try {
    const event = JSON.parse(data);
    return typeof event === 'object' && event !== null ? event as WritingAssistantStreamEvent : null;
  } catch {
    return null;
  }
}

async function responseError(response: Response): Promise<string> {
  const body = await response.json().catch(() => null) as { error?: string } | null;
  return body?.error || `Writing assistant request failed (${response.status})`;
}

/** One POST only: an error, abort or truncated stream is terminal for this click. */
export async function streamWritingAssistant(
  request: WritingAssistantRequest,
  options: StreamOptions = {},
): Promise<WritingAssistantDone> {
  const fetchImpl = options.fetchImpl ?? fetch;
  const response = await fetchImpl('/v1/assistant/write/stream', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(request),
    signal: options.signal,
  });
  if (!response.ok) throw new Error(await responseError(response));
  if (!response.body) throw new Error('Writing assistant stream is unavailable.');

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let carry = '';
  const consume = (frame: string): WritingAssistantDone | null => {
    const event = parseFrame(frame.trim());
    if (!event) return null;
    options.onEvent?.(event);
    if (event.error) throw new Error(event.error);
    if (!isDraft(event.draft)) return null;
    return {
      draft: event.draft,
      ...(event.audit && typeof event.audit === 'object' ? { audit: event.audit } : {}),
      ...(typeof event.text === 'string' ? { text: event.text } : {}),
      ...(event.receipt && typeof event.receipt === 'object' ? { receipt: event.receipt } : {}),
    };
  };

  for (;;) {
    const { done, value } = await reader.read();
    carry += decoder.decode(value, { stream: !done });
    let boundary = carry.search(/\r?\n\r?\n/);
    while (boundary !== -1) {
      const match = carry.slice(boundary).match(/^\r?\n\r?\n/);
      const separatorLength = match?.[0].length ?? 2;
      const result = consume(carry.slice(0, boundary));
      carry = carry.slice(boundary + separatorLength);
      if (result) {
        await reader.cancel().catch(() => undefined);
        return result;
      }
      boundary = carry.search(/\r?\n\r?\n/);
    }
    if (done) {
      const result = carry.trim() ? consume(carry) : null;
      if (result) return result;
      throw new Error('Writing assistant stream ended before a done frame.');
    }
  }
}
