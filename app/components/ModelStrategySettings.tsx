import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { AlertTriangle, CheckCircle2, Loader2, RefreshCw, Save, ShieldCheck, TestTube2 } from 'lucide-react';
import {
  ModelBindingsResponse,
  ProjectProfile,
  ProviderStrategy,
  readModelBindings,
  previewModelBindings,
  saveModelBindings,
} from '../services/modelBindings';

const SELECT = 'w-full rounded-lg border border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 outline-none focus:border-pink-500 dark:border-white/15 dark:bg-zinc-900 dark:text-white';

function routeLabel(strategy: ProviderStrategy): string {
  const ready = strategy.candidates?.filter((candidate) => candidate.ready).length ?? 0;
  const total = strategy.candidates?.length ?? 0;
  return `${strategy.display_name_zh} · ${strategy.route_id}${total ? ` · ${ready}/${total} 就绪` : ''}`;
}

function cloneProfile(profile: ProjectProfile): ProjectProfile {
  return JSON.parse(JSON.stringify(profile)) as ProjectProfile;
}

export const ModelStrategySettings: React.FC = () => {
  const [data, setData] = useState<ModelBindingsResponse | null>(null);
  const [draft, setDraft] = useState<ProjectProfile | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await readModelBindings();
      setData(next);
      setDraft(cloneProfile(next.profile));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '模型策略读取失败');
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const strategies = useMemo(() => ({
    text: (data?.strategies ?? []).filter((item) => item.capability_family === 'text'),
    music: (data?.strategies ?? []).filter((item) => item.capability_family === 'music'),
  }), [data]);

  const setCapability = (capability: 'text' | 'music', route: string) => {
    setDraft((current) => {
      if (!current) return current;
      const next = cloneProfile(current);
      next.capability_defaults[capability].selector = { type: 'route', id: route };
      return next;
    });
    setMessage(null);
  };

  const setRole = (roleId: string, capability: 'text' | 'music', route: string) => {
    setDraft((current) => {
      if (!current) return current;
      const next = cloneProfile(current);
      next.roles[roleId] = route === 'inherit'
        ? { capability, selector: { type: 'inherit' } }
        : { capability, selector: { type: 'route', id: route } };
      return next;
    });
    setMessage(null);
  };

  const preview = async () => {
    if (!draft) return;
    setBusy(true); setError(null); setMessage(null);
    try {
      await previewModelBindings(draft);
      setMessage('解析通过：这些业务角色都能解析到已发布的 OmniBridge 策略；没有调用模型，也不会产生生成费用。');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '策略解析失败');
    } finally { setBusy(false); }
  };

  const save = async () => {
    if (!draft || !data) return;
    setBusy(true); setError(null); setMessage(null);
    try {
      const next = cloneProfile(draft);
      next.profile_revision = data.profile.profile_revision + 1;
      const saved = await saveModelBindings(next, data.profile.profile_revision);
      setData({ ...data, profile: saved });
      setDraft(cloneProfile(saved));
      setMessage(`已保存项目模型策略 · Profile revision ${saved.profile_revision}`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '策略保存失败');
    } finally { setBusy(false); }
  };

  if (!data || !draft) {
    return <div className="flex min-h-48 items-center justify-center text-sm text-zinc-500">{busy ? <Loader2 className="animate-spin" /> : error || '模型策略不可用'}</div>;
  }

  return (
    <div className="max-w-3xl space-y-5">
      <section className="rounded-xl border border-indigo-500/20 bg-indigo-500/5 p-4">
        <div className="flex items-start gap-3">
          <ShieldCheck className="mt-0.5 shrink-0 text-indigo-500" size={19} />
          <div>
            <h4 className="text-sm font-bold text-zinc-900 dark:text-white">项目模型策略</h4>
            <p className="mt-1 text-xs leading-5 text-zinc-600 dark:text-zinc-300">这里配置 music-maker 的业务角色和 Route。供应商密钥、模型候选、顺序、健康检查与安全切换由 OmniBridge 中央管理，本项目不复制这些配置。</p>
            <p className="mt-2 font-mono text-[11px] text-zinc-500">Project Profile v2 · revision {data.profile.profile_revision}</p>
          </div>
        </div>
      </section>

      {!data.hub.available && (
        <p className="flex gap-2 rounded-lg bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-200"><AlertTriangle size={14} /> OmniBridge 策略目录暂不可用：{data.hub.error || '连接失败'}。现有 Profile 仍可查看，但暂不允许保存。</p>
      )}

      <section className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-white/10">
        <div>
          <h4 className="text-sm font-bold text-zinc-900 dark:text-white">能力默认策略</h4>
          <p className="mt-1 text-xs text-zinc-500">业务角色选择“继承默认”时使用这里的 Route。</p>
        </div>
        {(['text', 'music'] as const).map((capability) => (
          <label key={capability} className="block">
            <span className="mb-1 block text-xs font-semibold text-zinc-700 dark:text-zinc-200">{capability === 'text' ? '文本与文案' : '音乐生成'}</span>
            <select className={SELECT} value={draft.capability_defaults[capability].selector.id || ''} onChange={(event) => setCapability(capability, event.target.value)}>
              {strategies[capability].map((strategy) => <option key={strategy.route_id} value={strategy.route_id}>{routeLabel(strategy)}</option>)}
            </select>
          </label>
        ))}
      </section>

      <section className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-white/10">
        <div>
          <h4 className="text-sm font-bold text-zinc-900 dark:text-white">业务角色</h4>
          <p className="mt-1 text-xs text-zinc-500">推荐保持继承；只有明确需要更高质量或不同策略时才覆盖。</p>
        </div>
        <div className="divide-y divide-zinc-200 dark:divide-white/10">
          {data.roles.map((role) => {
            const binding = draft.roles[role.id];
            const value = binding?.selector.type === 'route' ? binding.selector.id || '' : 'inherit';
            const options = role.capability === 'music' ? strategies.music : strategies.text;
            return (
              <div key={role.id} className="grid gap-2 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(260px,1.2fr)] sm:items-center">
                <div>
                  <p className="text-sm font-semibold text-zinc-900 dark:text-white">{role.label_zh}</p>
                  <p className="text-xs text-zinc-500">{role.description_zh}</p>
                  <code className="text-[10px] text-zinc-400">{role.id}</code>
                </div>
                <select className={SELECT} value={value} onChange={(event) => setRole(role.id, role.capability, event.target.value)}>
                  <option value="inherit">自动继承能力默认（推荐）</option>
                  {options.map((strategy) => <option key={strategy.route_id} value={strategy.route_id}>{routeLabel(strategy)}</option>)}
                </select>
              </div>
            );
          })}
        </div>
      </section>

      {error && <p role="alert" className="flex gap-2 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-200"><AlertTriangle size={14} />{error}</p>}
      {message && <p className="flex gap-2 rounded-lg bg-emerald-500/10 px-3 py-2 text-xs text-emerald-700 dark:text-emerald-200"><CheckCircle2 size={14} />{message}</p>}

      <div className="flex flex-wrap gap-2">
        <button type="button" onClick={() => void preview()} disabled={busy || !data.hub.available} className="inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-2 text-xs font-semibold disabled:opacity-40 dark:border-white/15"><TestTube2 size={14} />只解析预览</button>
        <button type="button" onClick={() => void save()} disabled={busy || !data.hub.available} className="inline-flex items-center gap-2 rounded-lg bg-gradient-to-r from-orange-500 to-pink-600 px-3 py-2 text-xs font-bold text-white disabled:opacity-40">{busy ? <Loader2 size={14} className="animate-spin" /> : <Save size={14} />}保存项目策略</button>
        <button type="button" onClick={() => void load()} disabled={busy} className="inline-flex items-center gap-2 rounded-lg border border-zinc-300 px-3 py-2 text-xs font-semibold disabled:opacity-40 dark:border-white/15"><RefreshCw size={14} />重新读取</button>
      </div>
    </div>
  );
};
