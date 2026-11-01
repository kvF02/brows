// PE fixtures verify packaging rejection and integrity, not Windows execution.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { packageAlpha, verifyWindowsExecutable } from "./package-alpha.mjs";

const commit = "a".repeat(40);
function fixturePE() {
  const pe = Buffer.alloc(1024);
  pe.write("MZ"); pe.writeUInt32LE(128, 60);
  pe.writeUInt32LE(0x4550, 128); pe.writeUInt16LE(0x8664, 132); pe.writeUInt16LE(1, 134);
  pe.writeUInt16LE(240, 148); pe.writeUInt16LE(2, 150); pe.writeUInt16LE(0x20b, 152); pe.writeUInt16LE(2, 220);
  return pe;
}
async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "brows-alpha-package-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await Promise.all(["crates/desktop", "docs", "ui/dist"].map((part) => mkdir(join(root, part), { recursive: true })));
  await writeFile(join(root, "crates/desktop/tauri.alpha.conf.json"), JSON.stringify({ productName: "Brows Alpha", version: "0.1.0-alpha.1", identifier: "local-brows-desktop-alpha" }));
  await writeFile(join(root, "crates/desktop/tauri.conf.json"), JSON.stringify({ identifier: "local-brows-desktop" }));
  await writeFile(join(root, "docs/alpha-testing.md"), "测试说明 — fixture only\n");
  await writeFile(join(root, "ui/dist/index.html"), '<script src="/assets/app.js"></script>');
  const executable = join(root, "fixture.exe"); await writeFile(executable, fixturePE());
  return { repoRoot: root, executable, commit };
}

test("Alpha package carries exact binary, source revision, isolated namespace and verifiable checksums", async (t) => {
  const input = await fixture(t); const result = await packageAlpha(input);
  assert.deepEqual((await readdir(result.directory)).sort(), ["Brows-Alpha.exe", "README.zh-CN.md", "SHA256SUMS.txt", "build-info.json"].sort());
  assert.deepEqual(await readFile(result.executable), await readFile(input.executable));
  const info = JSON.parse(await readFile(join(result.directory, "build-info.json"), "utf8"));
  assert.equal(info.commit, commit); assert.equal(info.identifier, "local-brows-desktop-alpha");
  assert.equal(info.distribution, "unsigned-alpha"); assert.match(info.realBrowserAcceptance, /Not certified/);
  assert.equal(JSON.stringify(info).includes(input.repoRoot), false);
  const lines = (await readFile(join(result.directory, "SHA256SUMS.txt"), "utf8")).trim().split("\n");
  for (const line of lines) {
    const [expected, name] = line.split("  ");
    assert.equal(createHash("sha256").update(await readFile(join(result.directory, name))).digest("hex"), expected);
  }
});

test("packaging never substitutes a Linux, x86, DLL, console or malformed image", async (t) => {
  const input = await fixture(t);
  for (const variant of ["elf", "x86", "dll", "console", "offset", "truncated"]) {
    const bytes = fixturePE();
    if (variant === "elf") bytes.write("\x7fELF", 0, "ascii");
    if (variant === "x86") bytes.writeUInt16LE(0x14c, 132);
    if (variant === "dll") bytes.writeUInt16LE(0x2002, 150);
    if (variant === "console") bytes.writeUInt16LE(3, 220);
    if (variant === "offset") bytes.writeUInt32LE(0x7fffffff, 60);
    await writeFile(input.executable, variant === "truncated" ? bytes.subarray(0, 65) : bytes);
    await assert.rejects(packageAlpha(input));
  }
});

test("missing UI, untraceable commits or a shared data namespace refuse packaging", async (t) => {
  const input = await fixture(t);
  await assert.rejects(packageAlpha({ ...input, commit: "dirty" }), /source commit/);
  await writeFile(join(input.repoRoot, "ui/dist/index.html"), "not a built app");
  await assert.rejects(packageAlpha(input), /production UI/);
  await writeFile(join(input.repoRoot, "crates/desktop/tauri.alpha.conf.json"), JSON.stringify({ productName: "Brows Alpha", version: "0.1.0-alpha.1", identifier: "local-brows-desktop" }));
  await assert.rejects(packageAlpha(input), /isolated Alpha/);
});

test("repeated packages do not overwrite an earlier package or change its manifest", async (t) => {
  const input = await fixture(t);
  const first = await packageAlpha(input); const manifest = await readFile(join(first.directory, "build-info.json"));
  const second = await packageAlpha(input);
  assert.notEqual(first.directory, second.directory);
  assert.deepEqual(await readFile(join(first.directory, "build-info.json")), manifest);
});

test("a linked executable cannot supply package bytes", { skip: process.platform === "win32" ? "Windows symlink privilege is not required for packaging" : false }, async (t) => {
  const input = await fixture(t); const linked = join(input.repoRoot, "linked.exe");
  await symlink(input.executable, linked);
  await assert.rejects(verifyWindowsExecutable(linked), /regular Windows/);
});
