import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { crc32, inflateSync } from "node:zlib";
import test from "node:test";

test("packaging icon has valid PNG checksums and a decodable pixel stream", async () => {
  const png = Buffer.from((await readFile(new URL("../crates/desktop/icons/icon.png.base64", import.meta.url), "utf8")).trim(), "base64");
  assert.deepEqual(png.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  let offset = 8; const data = [];
  while (offset < png.length) {
    const size = png.readUInt32BE(offset);
    const chunk = png.subarray(offset + 4, offset + 8 + size);
    assert.equal(crc32(chunk), png.readUInt32BE(offset + 8 + size), chunk.subarray(0, 4).toString());
    if (chunk.subarray(0, 4).toString() === "IDAT") data.push(chunk.subarray(4));
    offset += size + 12;
  }
  assert.equal(offset, png.length);
  assert.equal(inflateSync(Buffer.concat(data)).length, 5);
});
