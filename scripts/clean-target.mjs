#!/usr/bin/env node
// 하루 한 번 target/debug 를 지운다 — 증분 캐시(incremental)·옛 deps 가 며칠이면 수십 GB 쌓인다.
//
//   node scripts/clean-target.mjs          마지막 정리가 하루 지났으면 지운다 (npm run tauri·release.mjs 가 부른다)
//   node scripts/clean-target.mjs --now    지금 지운다
//
// CARGO_TARGET_DIR 가 있으면 그곳도. cargo·rustc 가 돌고 있으면 건너뛴다 (빌드 중에 지우면 깨진다).
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const DAY = 24 * 60 * 60 * 1000;
const now = process.argv.includes("--now");

const targets = [...new Set([join(root, "target"), process.env.CARGO_TARGET_DIR && resolve(process.env.CARGO_TARGET_DIR)].filter(Boolean))];

function building() {
  try {
    const out = process.platform === "win32"
      ? execFileSync("tasklist", ["/fo", "csv", "/nh"], { encoding: "utf8" })
      : execFileSync("ps", ["-A", "-o", "comm="], { encoding: "utf8" });
    return /\b(cargo|rustc)(\.exe)?\b/i.test(out);
  } catch { return false; }
}

function size(dir) {
  let n = 0;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const f = join(dir, e.name);
    try { n += e.isDirectory() ? size(f) : statSync(f).size; } catch { /* 지우는 중·잠김 */ }
  }
  return n;
}

for (const t of targets) {
  const debug = join(t, "debug");
  const mark = join(t, ".last-clean");
  if (!existsSync(debug)) continue;
  if (!existsSync(mark)) { writeFileSync(mark, ""); if (!now) continue; } // 처음엔 날짜만 남긴다
  if (!now && Date.now() - statSync(mark).mtimeMs < DAY) continue;
  if (building()) { console.log(`정리 건너뜀: cargo 가 돌고 있다 (${debug})`); break; }
  const gb = (size(debug) / 1024 ** 3).toFixed(1);
  try {
    rmSync(debug, { recursive: true, force: true, maxRetries: 2 });
    writeFileSync(mark, "");
    console.log(`정리: ${debug} ${gb} GB 지움 — 이번 빌드는 처음부터라 몇 분 더 걸린다`);
  } catch (e) {
    // 띄워 둔 앱 exe 가 잠겨 있으면 일부만 지워진다 — 날짜는 그대로 두고 다음 빌드에 다시
    console.log(`정리 일부 실패 (${e.code ?? e.message}) — 띄운 앱을 끄면 다음 빌드에 다시: ${debug}`);
  }
}
