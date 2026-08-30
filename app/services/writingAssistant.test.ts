import { describe, expect, it, vi } from 'vitest';
import { streamWritingAssistant, type FetchLike, type WritingAssistantRequest } from './writingAssistant';

const REQUEST: WritingAssistantRequest = {
  target: 'all',
  description: '夜归',
  instruction: '合成器流行',
  lyrics: '',
  global_metadata: '',
  vocal_details: '',
  arrangement: '',
  duration_seconds: 60,
  instrumental: false,
  use_caption_rewriter: true,
};

function sseResponse(...chunks: string[]): Response {
  const encoder = new TextEncoder();
  return new Response(new ReadableStream<Uint8Array>({
    start(controller) {
      chunks.forEach((chunk) => controller.enqueue(encoder.encode(chunk)));
      controller.close();
    },
  }), { status: 200, headers: { 'Content-Type': 'text/event-stream' } });
}

describe('streamWritingAssistant', () => {
  it('uses exactly one POST and returns the structured done draft', async () => {
    const fetchImpl = vi.fn<FetchLike>().mockResolvedValue(sseResponse(
      'data: {"stage":"sent","model":"text-model"}\n\n',
      'data: {"stage":"done","draft":{"title":"霓虹夜航","lyrics":"[verse]\\n夜色"},"receipt":{"route_id":"route:text:quality"}}\n\n',
    ));
    const result = await streamWritingAssistant(REQUEST, { fetchImpl });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl).toHaveBeenCalledWith('/v1/assistant/write/stream', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify(REQUEST),
    }));
    expect(result.draft).toEqual({ title: '霓虹夜航', lyrics: '[verse]\n夜色' });
    expect(result.receipt?.route_id).toBe('route:text:quality');
  });

  it('forwards the simple-mode caption contract choice in the request body', async () => {
    const fetchImpl = vi.fn<FetchLike>().mockResolvedValue(sseResponse(
      'data: {"stage":"done","draft":{"lyrics":"[verse]\\nnight","global_metadata":"g","vocal_details":"v","arrangement":"a"}}\n\n',
    ));
    await streamWritingAssistant({ ...REQUEST, use_caption_rewriter: false }, { fetchImpl });
    const init = fetchImpl.mock.calls[0]?.[1];
    expect(JSON.parse(String(init?.body))).toMatchObject({
      target: 'all',
      use_caption_rewriter: false,
      lyrics: '',
    });
  });

  it('surfaces a stream error without replaying the request', async () => {
    const fetchImpl = vi.fn<FetchLike>().mockResolvedValue(sseResponse(
      'data: {"stage":"error","error":"route unavailable"}\n\n',
    ));
    await expect(streamWritingAssistant(REQUEST, { fetchImpl })).rejects.toThrow('route unavailable');
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('treats a truncated stream as terminal and does not call a fallback', async () => {
    const fetchImpl = vi.fn<FetchLike>().mockResolvedValue(sseResponse(
      'data: {"stage":"writing","delta":"partial"}\n\n',
    ));
    await expect(streamWritingAssistant(REQUEST, { fetchImpl })).rejects.toThrow('ended before a done frame');
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('propagates abort and never replays the POST', async () => {
    const controller = new AbortController();
    const fetchImpl = vi.fn<FetchLike>().mockImplementation((_input, init) => (
      new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true });
      })
    ));
    const pending = streamWritingAssistant(REQUEST, { fetchImpl, signal: controller.signal });
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });
});
