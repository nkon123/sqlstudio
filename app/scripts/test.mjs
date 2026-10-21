// 화면 코드의 단위 테스트 — src/**/*.test.ts 를 esbuild 로 묶어 node 로 돌린다 (npm test)
// 테스트 파일은 실패하면 예외를 던진다. 브라우저·Tauri 에 기대지 않는 모듈만 테스트한다.
import { build } from "esbuild";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";

const files = readdirSync("src", { recursive: true }).map(String).filter((f) => f.endsWith(".test.ts"));
const out = mkdtempSync(join(tmpdir(), "sqlstudio-test-"));
let failed = 0;
try {
  for (const f of files) {
    const outfile = join(out, f.replace(/[\\/]/g, "_").replace(/\.ts$/, ".mjs"));
    await build({ entryPoints: [join("src", f)], bundle: true, platform: "node", format: "esm", outfile, logLevel: "error" });
    try {
      await import(pathToFileURL(outfile).href);
      console.log(`ok   ${f}`);
    } catch (e) {
      failed++;
      console.error(`FAIL ${f}\n${e?.stack ?? e}`);
    }
  }
} finally {
  rmSync(out, { recursive: true, force: true });
}
if (failed) process.exit(1);
