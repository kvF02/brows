import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import vm from "node:vm";

// Test the real adapters without a webview, third-party runner, or emitted files.
export async function loadAdapter(name) {
  const context = vm.createContext({ console });
  const types = new vm.SourceTextModule(stripTypeScriptTypes(await readFile(new URL("../src/types.ts", import.meta.url), "utf8"), { mode: "transform" }), { context });
  const tauri = new vm.SyntheticModule(["invoke"], function () {
    this.setExport("invoke", () => { throw new Error("Unexpected real Tauri invocation"); });
  }, { context });
  const adapter = new vm.SourceTextModule(stripTypeScriptTypes(await readFile(new URL(`../src/api/${name}.ts`, import.meta.url), "utf8"), { mode: "transform" }), { context });
  await adapter.link((specifier) => {
    if (specifier === "../types") return types;
    if (specifier === "@tauri-apps/api/core") return tauri;
    throw new Error(`Unexpected import: ${specifier}`);
  });
  await adapter.evaluate();
  return adapter.namespace;
}
