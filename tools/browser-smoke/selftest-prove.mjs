// prove.mjs's pixel diff, without a browser: PNG encode → decode round-trips, one changed pixel
// of four is 25%, a size change counts the uncovered area, and Chrome-style filtered rows decode.
//   node .offrails/tools/browser-smoke/selftest-prove.mjs
import zlib from "node:zlib";
import { decodePng, diffPng, encodePng } from "./prove.mjs";

let failures = 0;
const check = (ok, what) => {
  if (!ok) failures += 1;
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
};
const image = (w, h, rgba) => ({ width: w, height: h, data: Buffer.from(Array.from({ length: w * h }, () => rgba).flat()) });

const a = image(2, 2, [10, 20, 30, 255]);
const back = decodePng(encodePng(a));
check(back.width === 2 && back.height === 2 && back.data.equals(a.data), "encode then decode gives the same pixels");
const b = image(2, 2, [10, 20, 30, 255]);
b.data[0] = 200;
check(diffPng(a, b).percent === 25, "one changed pixel of four is 25%");
check(diffPng(a, a).percent === 0, "identical images differ by 0%");
const tall = diffPng(a, image(2, 4, [10, 20, 30, 255]));
check(tall.percent === 50 && tall.resized, "a page that grew counts the new rows as changed");

// An RGB PNG with Sub, Up, Average and Paeth rows (what Chrome writes).
const w = 3;
const rows = [[1, 0, 0, 0, 5, 5, 5, 5, 5, 5], [2, 1, 1, 1, 1, 1, 1, 1, 1, 1], [3, 2, 2, 2, 2, 2, 2, 2, 2, 2], [4, 1, 1, 1, 0, 0, 0, 0, 0, 0]];
const raw = Buffer.from(rows.flat());
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(w, 0);
ihdr.writeUInt32BE(rows.length, 4);
ihdr[8] = 8;
ihdr[9] = 2;
const chunk = (kind, body) => {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(body.length, 0);
  head.write(kind, 4, "latin1");
  return Buffer.concat([head, body, Buffer.alloc(4)]);
};
const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
const d = decodePng(png);
const px = (x, y) => [...d.data.subarray((y * w + x) * 4, (y * w + x) * 4 + 4)];
check(JSON.stringify(px(2, 0)) === "[10,10,10,255]", "Sub rows add the pixel to the left");
check(JSON.stringify(px(0, 1)) === "[1,1,1,255]", "Up rows add the pixel above");
check(px(0, 2)[0] === 2 && px(1, 2)[0] === 6, "Average rows add the mean of left and above");
check(px(0, 3)[0] === 3 && px(1, 3)[0] === 6, "Paeth rows add the predicted neighbour");

console.log(failures ? `\nselftest-prove: ${failures} failure(s)` : "\nselftest-prove: ok");
process.exit(failures ? 1 : 0);
