import { readFile, writeFile } from "node:fs/promises";

// Keep the existing repository icon; wrap its PNG in the Windows ICO container.
const directory = new URL("../crates/desktop/icons/", import.meta.url);
const png = Buffer.from((await readFile(new URL("icon.png.base64", directory), "utf8")).trim(), "base64");
if (png.subarray(1, 4).toString() !== "PNG") throw new Error("Invalid repository icon");
const width = png.readUInt32BE(16), height = png.readUInt32BE(20);
if (width < 1 || width > 256 || height < 1 || height > 256) throw new Error("ICO dimensions exceed 256");
const header = Buffer.alloc(22);
header.writeUInt16LE(1, 2);
header.writeUInt16LE(1, 4);
header[6] = width === 256 ? 0 : width;
header[7] = height === 256 ? 0 : height;
header.writeUInt16LE(1, 10);
header.writeUInt16LE(32, 12);
header.writeUInt32LE(png.length, 14);
header.writeUInt32LE(22, 18);
await writeFile(new URL("icon.png", directory), png);
await writeFile(new URL("icon.ico", directory), Buffer.concat([header, png]));
