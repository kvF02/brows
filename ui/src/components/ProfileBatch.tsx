import { useEffect, useRef, useState, type FormEvent } from "react";
import type { ApiClient, Profile, ProxySummary } from "../types";
import { MAX_BATCH_PROFILES, parseBatchTags, runProfileBatch, type BatchAction, type BatchResult } from "../profile-batch";

export function useProfileBatch({ api, profiles, onRefresh, onError }: {
  api: ApiClient; profiles: Profile[]; onRefresh(): Promise<void>; onError(message: string): void;
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [results, setResults] = useState<BatchResult[]>([]);
  const [total, setTotal] = useState(0);
  const active = useRef(false), cancel = useRef(false), mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; cancel.current = true; }; }, []);
  useEffect(() => {
    const visible = new Set(profiles.map(profile => profile.id));
    setSelected(previous => {
      const keep = new Set([...previous].filter(id => visible.has(id)));
      return keep.size === previous.size ? previous : keep;
    });
  }, [profiles]);
  const toggle = (id: string) => {
    if (active.current) return;
    setSelected(previous => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else if (next.size < MAX_BATCH_PROFILES) next.add(id);
      return next;
    });
  };
  const selectVisible = () => {
    if (active.current) return;
    const ids = profiles.slice(0, MAX_BATCH_PROFILES).map(profile => profile.id);
    setSelected(ids.length > 0 && ids.every(id => selected.has(id)) ? new Set() : new Set(ids));
  };
  const execute = async (action: BatchAction) => {
    if (active.current) return;
    const byId = new Map(profiles.map(profile => [profile.id, profile]));
    const snapshot = [...selected].flatMap(id => { const profile = byId.get(id); return profile ? [profile] : []; });
    if (!snapshot.length) return;
    active.current = true; cancel.current = false; setBusy(true); setResults([]); setTotal(snapshot.length); setEditing(false);
    try {
      await runProfileBatch(api, snapshot, action, { cancelled: () => cancel.current,
        progress: updated => { if (mounted.current) setResults(updated); } });
    } catch (cause) { if (mounted.current) onError(cause instanceof Error ? cause.message : "批量操作失败"); }
    finally {
      try { await onRefresh(); } catch { if (mounted.current) onError("操作已执行，但列表刷新失败，请手动刷新"); }
      active.current = false; if (mounted.current) setBusy(false);
    }
  };
  return { selected, busy, editing, setEditing, results, total, toggle, selectVisible, execute,
    allVisible: profiles.length > 0 && profiles.slice(0, MAX_BATCH_PROFILES).every(profile => selected.has(profile.id)),
    clear: () => { if (!active.current) setSelected(new Set()); },
    cancel: () => { cancel.current = true; },
  };
}

export function ProfileBatchControls({ batch, proxies }: {
  batch: ReturnType<typeof useProfileBatch>; proxies: ProxySummary[];
}) {
  const [tags, setTags] = useState(""), [proxy, setProxy] = useState("keep"), [formError, setFormError] = useState("");
  useEffect(() => { if (batch.editing) { setTags(""); setProxy("keep"); setFormError(""); } }, [batch.editing]);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    try {
      const addTags = parseBatchTags(tags);
      if (!addTags.length && proxy === "keep") throw new Error("请选择要追加的标签或代理设置");
      setFormError("");
      void batch.execute({ kind: "edit", addTags, ...(proxy === "keep" ? {} : { proxyId: proxy === "direct" ? null : proxy.slice(6) }) });
    } catch (cause) { setFormError(cause instanceof Error ? cause.message : "输入有误"); }
  };
  const count = batch.selected.size;
  const selectAll = useRef<HTMLInputElement>(null);
  useEffect(() => { if (selectAll.current) selectAll.current.indeterminate = count > 0 && !batch.allVisible; }, [count, batch.allVisible]);
  return <div className="profile-batch" aria-label="批量操作">
    <div className="batch-toolbar">
      <label className="batch-select-all"><input ref={selectAll} type="checkbox" aria-label="选择当前结果" checked={batch.allVisible} disabled={batch.busy} onChange={batch.selectVisible} />选择当前结果</label>
      <span className="batch-count">已选 {count} · 每批最多 {MAX_BATCH_PROFILES} 个</span>
      <button type="button" className="secondary-button small" disabled={batch.busy || !count} onClick={() => void batch.execute({ kind: "launch" })}>批量启动</button>
      <button type="button" className="secondary-button small" disabled={batch.busy || !count} onClick={() => void batch.execute({ kind: "stop" })}>批量停止</button>
      <button type="button" className="secondary-button small" disabled={batch.busy || !count} onClick={() => { setFormError(""); batch.setEditing(true); }}>批量编辑</button>
      <button type="button" className="text-button" disabled={batch.busy || !count} onClick={batch.clear}>清除选择</button>
      {batch.busy && <span className="batch-count">离开此页会停止剩余操作</span>}
      {batch.busy && <button type="button" className="text-button" onClick={batch.cancel}>停止后续操作</button>}
    </div>
    {batch.results.length > 0 && <div className="batch-results" aria-live="polite">
      <strong>{batch.busy ? "执行中" : "批量操作完成"} {batch.results.length}/{batch.total}</strong>
      <span>成功 {batch.results.filter(r => r.status === "success").length} · 跳过 {batch.results.filter(r => r.status === "skipped").length} · 失败 {batch.results.filter(r => r.status === "failed").length} · 未执行 {batch.results.filter(r => r.status === "cancelled").length}</span>
      <ul>{batch.results.map(result => <li key={result.id} className={`batch-result-${result.status}`}><strong>{result.name}</strong><span>{result.message}</span></li>)}</ul>
    </div>}
    {batch.editing && <div className="modal-backdrop" role="presentation" onMouseDown={event => { if (event.target === event.currentTarget) batch.setEditing(false); }}>
      <section className="modal batch-edit-modal" role="dialog" aria-modal="true" aria-labelledby="batch-edit-title">
        <div className="modal-head"><h2 id="batch-edit-title">批量编辑 {count} 个环境</h2><button type="button" className="icon-button" aria-label="关闭批量编辑" onClick={() => batch.setEditing(false)}>×</button></div>
        <p className="modal-intro">追加标签、切换代理；仅处理已停止或异常的环境。每个环境保留自己的指纹种子、语言和运行时。</p>
        <form onSubmit={submit}>
          <label className="form-field"><span>追加标签</span><input className="text-input" id="batch-tags" value={tags} maxLength={1024} onChange={e => setTags(e.target.value)} placeholder="逗号分隔，例如：测试, 亚洲" /></label>
          <label className="form-field"><span>代理设置</span><select className="text-input" id="batch-proxy" value={proxy} onChange={e => setProxy(e.target.value)}>
            <option value="keep">保持各自代理</option><option value="direct">使用系统网络</option>
            {proxies.map(item => <option key={item.id} value={`proxy:${item.id}`}>{item.name} · {item.kind.toUpperCase()}</option>)}
          </select></label>
          {formError && <p className="form-error" role="alert">{formError}</p>}
          <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => batch.setEditing(false)}>取消</button><button type="submit" className="primary-button" disabled={!count}>应用批量修改</button></div>
        </form>
      </section>
    </div>}
  </div>;
}
