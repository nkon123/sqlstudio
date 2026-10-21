#!/usr/bin/env node
// 릴리스 준비 — 버전 다섯 곳을 한 번에 올리고, 릴리스 노트 파일(docs/release-notes/vX.md)을 챙긴다.
//
//   node scripts/release.mjs patch          0.5.27 → 0.5.28 (minor·major·0.6.0 처럼 직접도)
//   node scripts/release.mjs --check [v0.5.28]   버전 다섯 곳이 같은지, 노트 파일이 있는지 (릴리스 CI 가 태그로 부른다)
//
// 노트 파일이 없으면 지난 태그 이후 커밋 제목으로 초안을 만든다 — 다듬은 뒤 커밋하고 태그를 올린다.
// 릴리스 CI 는 이 파일을 릴리스 본문으로 쓴다 (없으면 멈춘다).
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const p = (f) => join(root, f);
const read = (f) => readFileSync(p(f), "utf8");
const write = (f, s) => writeFileSync(p(f), s);

/** 버전이 적힌 곳 — [파일, 읽기, 바꾸기] */
const SPOTS = [
  ["app/src-tauri/tauri.conf.json", (s) => JSON.parse(s).version, (s, v) => s.replace(/("version"\s*:\s*")[^"]+(")/, `$1${v}$2`)],
  ["app/package.json", (s) => JSON.parse(s).version, (s, v) => s.replace(/("version"\s*:\s*")[^"]+(")/, `$1${v}$2`)],
  // package-lock: 맨 위와 packages[""] 두 곳
  ["app/package-lock.json", (s) => { const j = JSON.parse(s); return j.version === j.packages?.[""]?.version ? j.version : `${j.version}≠${j.packages?.[""]?.version}`; },
    (s, v) => { let n = 0; return s.replace(/("version"\s*:\s*")[^"]+(")/g, (m, a, b) => (n++ < 2 ? `${a}${v}${b}` : m)); }],
  ["app/src-tauri/Cargo.toml", (s) => /^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m.exec(s)?.[1], (s, v) => s.replace(/^(\[package\][\s\S]*?^version\s*=\s*")[^"]+(")/m, `$1${v}$2`)],
  ["Cargo.lock", (s) => /\[\[package\]\]\nname = "sqlstudio"\nversion = "([^"]+)"/.exec(s.replace(/\r\n/g, "\n"))?.[1],
    (s, v) => s.replace(/(\[\[package\]\]\r?\nname = "sqlstudio"\r?\nversion = ")[^"]+(")/, `$1${v}$2`)],
];

const versions = () => SPOTS.map(([f, get]) => [f, get(read(f))]);
const notesPath = (v) => `docs/release-notes/v${v}.md`;

function check(tag) {
  const vs = versions();
  const want = tag ? tag.replace(/^v/, "") : vs[0][1];
  const bad = vs.filter(([, v]) => v !== want);
  for (const [f, v] of bad) console.error(`버전이 다릅니다: ${f} = ${v} (기대 ${want})`);
  const notes = notesPath(want);
  if (!existsSync(p(notes))) console.error(`릴리스 노트가 없습니다: ${notes} — node scripts/release.mjs 로 초안을 만들고 다듬어 커밋하세요`);
  else if (!read(notes).trim()) console.error(`릴리스 노트가 비었습니다: ${notes}`);
  else if (read(notes).includes("<!-- 초안")) console.error(`릴리스 노트가 아직 초안입니다 (맨 위 '초안' 줄을 지우고 다듬으세요): ${notes}`);
  const ok = !bad.length && existsSync(p(notes)) && read(notes).trim() && !read(notes).includes("<!-- 초안");
  if (ok) console.log(`버전 ${want} · 노트 ${notes} 확인`);
  process.exit(ok ? 0 : 1);
}

function next(cur, arg) {
  const [ma, mi, pa] = cur.split(".").map(Number);
  if (arg === "patch") return `${ma}.${mi}.${pa + 1}`;
  if (arg === "minor") return `${ma}.${mi + 1}.0`;
  if (arg === "major") return `${ma + 1}.0.0`;
  if (/^\d+\.\d+\.\d+$/.test(arg)) return arg;
  console.error("사용: node scripts/release.mjs patch|minor|major|X.Y.Z   또는  --check [vX.Y.Z]");
  process.exit(2);
}

/** 지난 태그 이후 커밋 제목으로 노트 초안 */
function draftNotes(v) {
  const git = (...a) => execFileSync("git", a, { cwd: root, encoding: "utf8" }).trim();
  let last = "";
  try { last = git("describe", "--tags", "--abbrev=0"); } catch { /* 태그가 없다 */ }
  const log = git("log", "--format=%s", last ? `${last}..HEAD` : "HEAD").split("\n")
    .filter((s) => s && !/^\d+\.\d+\.\d+$/.test(s));
  return [
    `<!-- 초안 — 지난 태그(${last || "처음"}) 이후 커밋 제목. 기능별 '## 제목' 과 굵은 요약으로 다듬는다 (이 줄은 지운다) -->`,
    "",
    ...log.map((s) => `- ${s}`),
    "",
  ].join("\n");
}

const args = process.argv.slice(2);
if (args[0] === "--check") check(args[1]);

const cur = versions()[0][1];
const v = next(cur, args[0]);
for (const [f, , set] of SPOTS) {
  const before = read(f);
  const after = set(before, v);
  if (after === before && !before.includes(v)) { console.error(`${f} 의 버전을 찾지 못했습니다`); process.exit(1); }
  write(f, after);
}
console.log(`버전 ${cur} → ${v} (${SPOTS.map(([f]) => f).join(", ")})`);
const notes = notesPath(v);
if (!existsSync(p(notes))) {
  mkdirSync(p("docs/release-notes"), { recursive: true });
  write(notes, draftNotes(v));
  console.log(`노트 초안: ${notes} — 다듬은 뒤 커밋하고 태그 v${v} 를 올리세요`);
} else {
  console.log(`노트: ${notes} (이미 있음)`);
}
