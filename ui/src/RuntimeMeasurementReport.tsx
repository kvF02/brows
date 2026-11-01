import type { FingerprintMeasurementReport, FingerprintObservation, FingerprintSignals } from "./types";

const shown = (value: unknown) => value == null ? "未取得" : String(value);
const screen = (s: FingerprintSignals) => `${shown(s.screenWidth)} × ${shown(s.screenHeight)} / ${shown(s.colorDepth)} bit`;
const storage = (o: FingerprintObservation) => `Local ${o.storage.localBefore} → ${o.storage.localAfter}；Cookie ${o.storage.cookieBefore} → ${o.storage.cookieAfter}`;
const labels = { pass: "通过", fail: "未通过", observed: "仅记录", not_covered: "未覆盖", not_measured: "未测量" };
const scopes = { isolated: "隔离", configurable: "可配置", host_shared: "主机共享" };

export function RuntimeMeasurementReport({ report }: { report: FingerprintMeasurementReport }) {
  return <section className="measurement-report" aria-label="双临时环境实测报告">
    <div className={report.coveredChecksPassed ? "form-info" : "form-error"} role="status">
      <strong>{report.coveredChecksPassed ? "覆盖项通过" : "覆盖项存在失败"}</strong>
      <p>两个临时样本 · {report.mode} · {new Date(report.measuredAtMs).toLocaleString()} · 版本 {report.runtimeVersion}</p>
      <small>本次运行时 SHA-256：{report.runtimeSha256}</small>
    </div>
    {report.samples.map((sample) => {
      const c = sample.configured;
      const rows: Array<[string, string, (o: FingerprintObservation) => string]> = [
        ["UA", c.userAgent, (o) => shown(o.page.userAgent)],
        ["平台 / 语言", `${c.platform} / ${c.languages.join(", ")}`, (o) => `${shown(o.page.platform)} / ${o.page.languages.join(", ")}`],
        ["CPU / Memory", `${c.concurrency} / ${c.memory} GB (number)`, (o) => `${shown(o.page.concurrency)} / ${shown(o.page.memory)} GB (${o.page.memoryType})`],
        ["Screen", `${c.screenWidth} × ${c.screenHeight} / ${c.colorDepth} bit`, (o) => screen(o.page)],
        ["WebGL 声明", `${c.webglVendor} / ${c.webglRenderer}`, (o) => `${shown(o.page.webglVendor)} / ${shown(o.page.webglRenderer)}`],
        ["Canvas 摘要", `固定 seed：${c.seed}`, (o) => `read ${shown(o.page.canvas?.readHash)} / export ${shown(o.page.canvas?.exportHash)}`],
        ["离线 Audio 摘要", "同一样本重复、重启后稳定", (o) => `Promise ${shown(o.audio?.firstHash)} / event ${shown(o.audio?.firstEventHash)}`],
        ["专用测试存储", "首次 empty；重启 own；写入后 own", storage],
      ];
      return <details key={sample.label} open><summary>临时样本 {sample.label}：配置与真实读数</summary>
        <div className="measurement-table"><table><thead><tr><th>字段</th><th>配置 / 预期</th><th>首次同时启动</th><th>正常退出后重启</th></tr></thead>
          <tbody>{rows.map(([name, expected, read]) => <tr key={name}><th>{name}</th><td>{expected}</td>{[0, 1].map((round) => <td key={round}>{sample.observations[round] ? read(sample.observations[round]) : "未取得"}</td>)}</tr>)}</tbody>
        </table></div>
      </details>;
    })}
    <details open><summary>逐项判定与覆盖边界</summary>
      {report.checks.map((check) => <div className="measurement-check" key={check.key}>
        <strong className={check.status === "fail" ? "error-text" : undefined}>{labels[check.status]} · {check.label}</strong>
        <small>{scopes[check.scope]} · {check.required ? "影响覆盖项结论" : "独立列示"}</small>
        {check.expected && <p>预期：{check.expected}</p>}{check.observed && <p>实测：{check.observed}</p>}
        <p>{check.explanation}</p>
      </div>)}
    </details>
    <ul>{report.limitations.map((line) => <li key={line}>{line}</li>)}</ul>
    <details><summary>完整观测 JSON（含 iframe / Worker / UA-CH）</summary><pre>{JSON.stringify(report, null, 2)}</pre></details>
  </section>;
}
