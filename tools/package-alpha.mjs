// Packaging validates format and integrity; it does not execute Windows code.
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { copyFile, lstat, mkdir, mkdtemp, open, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const executableName = "Brows-Alpha.exe";
const utf8 = (path) => readFile(path, "utf8");
async function hash(path) {
  const value = createHash("sha256");
  for await (const chunk of createReadStream(path)) value.update(chunk);
  return value.digest("hex");
}
export async function verifyWindowsExecutable(path) {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size < 512 || info.size > 512 * 1024 * 1024) throw new Error("Expected a regular Windows x64 executable");
  const file = await open(path, "r");
  try {
    const dos = Buffer.alloc(64);
    if ((await file.read(dos, 0, dos.length, 0)).bytesRead !== dos.length || dos.toString("ascii", 0, 2) !== "MZ") throw new Error("Missing DOS header");
    const offset = dos.readUInt32LE(60);
    const pe = Buffer.alloc(112);
    if (offset < 64 || offset > 1024 * 1024 || offset + pe.length > info.size || (await file.read(pe, 0, pe.length, offset)).bytesRead !== pe.length) throw new Error("Invalid PE header offset");
    if (pe.readUInt32LE(0) !== 0x4550 || pe.readUInt16LE(4) !== 0x8664 || pe.readUInt16LE(6) < 1 || pe.readUInt16LE(6) > 96 || pe.readUInt16LE(20) < 112 || pe.readUInt16LE(24) !== 0x20b || offset + 24 + pe.readUInt16LE(20) + pe.readUInt16LE(6) * 40 > info.size) throw new Error("Expected AMD64 PE32+ format");
    const flags = pe.readUInt16LE(22);
    if (!(flags & 2) || (flags & 0x2000) || pe.readUInt16LE(24 + 68) !== 2) throw new Error("Expected a release Windows GUI executable, not a DLL or console build");
  } finally { await file.close(); }
}

export async function packageAlpha({ repoRoot, executable, commit }) {
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error("A full source commit is required");
  const [alpha, stable] = await Promise.all(["tauri.alpha.conf.json", "tauri.conf.json"].map(async (name) => JSON.parse(await utf8(join(repoRoot, "crates/desktop", name)))));
  if (alpha.identifier !== "local-brows-desktop-alpha" || alpha.identifier === stable.identifier || alpha.productName !== "Brows Alpha" || !/^\d+\.\d+\.\d+-alpha\.\d+$/.test(alpha.version) || Object.keys(alpha).length !== 3) throw new Error("Invalid isolated Alpha configuration");
  await verifyWindowsExecutable(executable);
  const [notes, frontend] = await Promise.all([utf8(join(repoRoot, "docs/alpha-testing.md")), readFile(join(repoRoot, "ui/dist/index.html"))]);
  if (!frontend.includes(Buffer.from("assets/"))) throw new Error("Build the production UI before packaging");
  const root = join(repoRoot, "target/test-builds");
  await mkdir(root, { recursive: true });
  const name = `brows-${alpha.version}-windows-x64-${commit.slice(0, 12)}`;
  const directory = await mkdtemp(join(root, `${name}-`));
  try {
    const output = join(directory, executableName);
    const before = await hash(executable);
    await copyFile(executable, output);
    await verifyWindowsExecutable(output);
    if (await hash(output) !== before || await hash(executable) !== before) throw new Error("Executable changed during packaging");
    await writeFile(join(directory, "README.zh-CN.md"), notes, "utf8");
    const metadata = {
      schemaVersion: 1, product: alpha.productName, version: alpha.version, commit,
      builtAtUtc: new Date().toISOString(), platform: "windows-x64", identifier: alpha.identifier,
      executable: executableName, executableSha256: before,
      frontendIndexSha256: createHash("sha256").update(frontend).digest("hex"),
      distribution: "unsigned-alpha", automatedChecks: "See the build log for executed checks",
      realBrowserAcceptance: "Not certified by packaging; run the local runtime probe and consult the separate Windows fingerprint job",
    };
    await writeFile(join(directory, "build-info.json"), `${JSON.stringify(metadata, null, 2)}\n`, "utf8");
    const sums = await Promise.all([executableName, "README.zh-CN.md", "build-info.json"].map(async (name) => `${await hash(join(directory, name))}  ${name}`));
    await writeFile(join(directory, "SHA256SUMS.txt"), `${sums.join("\n")}\n`, "utf8");
    return { directory, artifactName: name, executable: output, commit };
  } catch (error) { await rm(directory, { recursive: true, force: true }); throw error; }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [executable, commit, ...extra] = process.argv.slice(2);
  if (!executable || !commit || extra.length) throw new Error("Usage: node tools/package-alpha.mjs <Windows executable> <full commit>");
  const result = await packageAlpha({ repoRoot: resolve(dirname(fileURLToPath(import.meta.url)), ".."), executable, commit });
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
