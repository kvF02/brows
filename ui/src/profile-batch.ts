import type { ApiClient, Profile, ProfileInput } from "./types";

export const MAX_BATCH_PROFILES = 200;
export type BatchAction = { kind: "launch" } | { kind: "stop" } | {
  kind: "edit"; addTags: string[]; proxyId?: string | null;
};
export interface BatchResult {
  id: string; name: string; status: "success" | "skipped" | "failed" | "cancelled"; message: string;
}
const stopped = (profile: Profile) => profile.status === "stopped" || profile.status === "failed";
const asciiLower = (value: string) => value.replace(/[A-Z]/g, c => c.toLowerCase());
export function parseBatchTags(value: string): string[] {
  const tags = value.split(/[,，\n]/).map(tag => tag.trim()).filter(Boolean);
  if (tags.some(tag => new TextEncoder().encode(tag).length > 64)) throw new Error("每个标签最多 64 字节，请缩短标签");
  return [...new Map(tags.map(tag => [asciiLower(tag), tag])).values()];
}
export async function runProfileBatch(
  api: Pick<ApiClient, "launchSession" | "stopSession" | "updateProfile">,
  selected: Profile[], action: BatchAction,
  options: { cancelled?(): boolean; progress?(results: BatchResult[]): void } = {},
): Promise<BatchResult[]> {
  if (!selected.length || selected.length > MAX_BATCH_PROFILES) throw new Error(`每批请选择 1–${MAX_BATCH_PROFILES} 个环境`);
  if (action.kind === "edit" && !action.addTags.length && action.proxyId === undefined) throw new Error("请选择要追加的标签或代理设置");
  const profiles = [...new Map(selected.map(profile => [profile.id, profile])).values()];
  // Cancel selected queue tickets before stopping active browsers, which can promote tickets.
  if (action.kind === "stop") profiles.sort((a, b) => Number(b.status === "queued") - Number(a.status === "queued"));
  const results: BatchResult[] = [];
  for (const profile of profiles) {
    const result: BatchResult = { id: profile.id, name: profile.name, status: "success", message: "" };
    if (options.cancelled?.()) { result.status = "cancelled"; result.message = "未执行"; }
    else if ((action.kind === "launch" || action.kind === "edit") && !stopped(profile)) {
      result.status = "skipped"; result.message = "仅处理已停止或异常的环境";
    } else if (action.kind === "stop" && !["queued", "running", "starting"].includes(profile.status)) {
      result.status = "skipped"; result.message = "没有需要停止的会话";
    } else {
      try {
        if (action.kind === "launch") {
          const changed = await api.launchSession(profile.id);
          if (!["running", "starting", "queued"].includes(changed.status)) throw new Error("启动未生效，请查看环境状态");
          result.message = changed.status === "queued" ? "已排队" : "已启动";
        } else if (action.kind === "stop") {
          const changed = await api.stopSession(profile.id);
          if (changed.status !== "stopped") throw new Error("停止未完成，请查看环境状态");
          result.message = "已停止";
        } else {
          const changes: Partial<ProfileInput> = {};
          if (action.addTags.length) {
            const tags = [...profile.tags]; const seen = new Set(tags.map(asciiLower));
            for (const tag of action.addTags) if (!seen.has(asciiLower(tag))) { tags.push(tag); seen.add(asciiLower(tag)); }
            if (tags.length !== profile.tags.length) changes.tags = tags;
          }
          if (action.proxyId !== undefined && (action.proxyId || undefined) !== (profile.proxyId || undefined)) changes.proxyId = action.proxyId ?? "";
          if (Object.keys(changes).length) {
            await api.updateProfile(profile.id, profile.revision, changes);
            result.message = "已保存";
          } else { result.status = "skipped"; result.message = "配置已一致"; }
        }
      } catch (cause) {
        result.status = "failed";
        result.message = cause instanceof Error ? cause.message : "操作失败，请刷新后重试";
      }
    }
    results.push(result); options.progress?.([...results]);
  }
  return results;
}
