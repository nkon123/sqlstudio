// 제3자 라이선스 고지 파일을 만든다 → public/third-party-notices.html (앱 번들과 설치 폴더에 들어간다)
//
// - Rust: cargo metadata 로 배포 바이너리에 링크되는 크레이트(빌드·개발 전용 제외)를 모으고, 각 크레이트의 LICENSE*/COPYING*/NOTICE* 를 읽는다
// - 프런트엔드: npm 의 production 의존성
// - 같은 라이선스 전문은 한 번만 싣고 그 전문을 쓰는 패키지를 나열한다
// 릴리스 빌드(npm run build) 때마다 다시 만든다.

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(app, "..");
const OUT = join(app, "public", "third-party-notices.html");
const LICENSE_FILE = /^(licen[cs]e|copying|notice|unlicense)([-_.].*)?$/i;

/** @type {{ name: string, version: string, license: string, repo: string, eco: string, texts: string[] }[]} */
const pkgs = [];

function filesIn(dir) {
  return readdirSync(dir, { withFileTypes: true })
    .filter((f) => f.isFile() && LICENSE_FILE.test(f.name) && !/\.(rs|js|ts|json|toml|html)$/i.test(f.name))
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((f) => readFileSync(join(dir, f.name), "utf8").replace(/\r\n/g, "\n").trim())
    .filter(Boolean);
}

/** 맨 위 → 한 단계 아래 폴더(포함한 C 라이브러리 등, 예: oracle 의 odpi/) → README 의 License 절 */
function licenseTexts(dir) {
  if (!existsSync(dir)) return [];
  const t = filesIn(dir);
  if (t.length) return t;
  for (const d of readdirSync(dir, { withFileTypes: true })) {
    if (d.isDirectory() && !/^(src|tests?|examples?|benches|docs?|node_modules|target)$/i.test(d.name)) t.push(...filesIn(join(dir, d.name)));
  }
  if (t.length) return t;
  const readme = readdirSync(dir).find((f) => /^readme(\.md)?$/i.test(f));
  if (readme) {
    const m = readFileSync(join(dir, readme), "utf8").replace(/\r\n/g, "\n").match(/^#{1,3}\s*Licen[cs]e.*\n([\s\S]*?)(?=\n#{1,3}\s|$(?![\s\S]))/im);
    if (m && m[1].trim()) return [`(README 의 License 절)\n${m[1].trim()}`];
  }
  return [];
}

// ── Rust ──
const meta = JSON.parse(execFileSync("cargo", ["metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"],
  { cwd: root, maxBuffer: 256 * 1024 * 1024, encoding: "utf8" }));
const byId = new Map(meta.packages.map((p) => [p.id, p]));
const resolveNodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
const seen = new Set();
const stack = meta.packages.filter((p) => ["sqlstudio", "sqls-mcp", "sqls-analyze"].includes(p.name)).map((p) => p.id);
while (stack.length) {
  const id = stack.pop();
  if (seen.has(id)) continue;
  seen.add(id);
  for (const d of resolveNodes.get(id)?.deps ?? []) if (d.dep_kinds.some((k) => k.kind === null)) stack.push(d.pkg);
}
for (const id of seen) {
  const p = byId.get(id);
  if (!p.source) continue; // 이 저장소의 크레이트
  pkgs.push({ name: p.name, version: p.version, license: p.license ?? "(파일 참조)", repo: p.repository ?? `https://crates.io/crates/${p.name}`,
    eco: "Rust", texts: licenseTexts(dirname(p.manifest_path)) });
}

// ── npm ──
const paths = execFileSync("npm ls --omit=dev --all --parseable", { cwd: app, encoding: "utf8", shell: true })
  .split(/\r?\n/).map((s) => s.trim()).filter((s) => s && resolve(s) !== app);
const npmSeen = new Set();
for (const dir of paths) {
  const pj = join(dir, "package.json");
  if (!existsSync(pj)) continue;
  const j = JSON.parse(readFileSync(pj, "utf8"));
  const key = `${j.name}@${j.version}`;
  if (npmSeen.has(key)) continue;
  npmSeen.add(key);
  const repo = typeof j.repository === "string" ? j.repository : j.repository?.url;
  pkgs.push({ name: j.name, version: j.version, license: typeof j.license === "string" ? j.license : j.license?.type ?? "(파일 참조)",
    repo: (repo ?? `https://www.npmjs.com/package/${j.name}`).replace(/^git\+/, "").replace(/\.git$/, "").replace(/^github:/, "https://github.com/"),
    eco: "npm", texts: licenseTexts(dir) });
}

pkgs.sort((a, b) => a.eco.localeCompare(b.eco) || a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

// 같은 전문 묶기 (공백 차이는 무시)
const groups = new Map();
for (const p of pkgs) for (const t of p.texts) {
  const k = t.replace(/\s+/g, " ");
  if (!groups.has(k)) groups.set(k, { text: t, users: [] });
  groups.get(k).users.push(`${p.name} ${p.version}`);
}
const missing = pkgs.filter((p) => !p.texts.length);

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const link = (u) => /^https?:\/\//.test(u) ? `<a href="${esc(u)}" target="_blank" rel="noreferrer">${esc(u)}</a>` : esc(u);
const html = `<!doctype html>
<html lang="ko"><head><meta charset="utf-8"><title>오픈소스 라이선스 고지</title>
<style>
body { font: 13px/1.5 "Malgun Gothic", system-ui, sans-serif; margin: 16px; color: #1d232b; background: #fff; }
@media (prefers-color-scheme: dark) { body { color: #dfe3e8; background: #1d2127; } a { color: #7fb0ff; } pre { background: #16191e !important; } td, th { border-color: #313740 !important; } }
h1 { font-size: 18px; } h2 { font-size: 15px; margin-top: 28px; }
table { border-collapse: collapse; font-size: 12px; width: 100%; }
th, td { border-bottom: 1px solid #d9dde3; padding: 2px 6px; text-align: left; vertical-align: top; }
td a { word-break: break-all; }
pre { white-space: pre-wrap; background: #f6f7f9; padding: 8px; border-radius: 4px; font: 11px/1.4 Consolas, monospace; }
.users { font-size: 12px; color: #6b7480; }
</style></head><body>
<h1>오픈소스 라이선스 고지</h1>
<p>SQLStudio 에는 아래 오픈소스 소프트웨어가 포함되어 있습니다. 각 소프트웨어는 해당 라이선스를 따르며, 저작권은 각 저작자에게 있습니다.
각 구성 요소의 소스 코드는 표의 저장소 주소에서 받을 수 있습니다. 특히 MPL-2.0 및 EPL-2.0 구성 요소(cssparser, selectors, dtoa-short, option-ext, elkjs 등)는
수정 없이 사용했으며, 그 소스 코드는 표에 적힌 원 저장소(해당 버전)에서 받을 수 있습니다.</p>
<p>설치 파일에는 Microsoft Edge WebView2 Runtime 이 포함될 수 있으며, 이는 Microsoft 의 사용 조건에 따라 재배포됩니다
(<a href="https://developer.microsoft.com/microsoft-edge/webview2/" target="_blank" rel="noreferrer">https://developer.microsoft.com/microsoft-edge/webview2/</a>).
Oracle Instant Client 는 포함되어 있지 않습니다.</p>
<h2>구성 요소 (${pkgs.length}개)</h2>
<table><thead><tr><th>이름</th><th>버전</th><th>라이선스</th><th>구분</th><th>소스</th></tr></thead><tbody>
${pkgs.map((p) => `<tr><td>${esc(p.name)}</td><td>${esc(p.version)}</td><td>${esc(p.license)}</td><td>${p.eco}</td><td>${link(p.repo)}</td></tr>`).join("\n")}
</tbody></table>
<h2>라이선스 전문</h2>
${[...groups.values()].map((g) => `<p class="users">${esc(g.users.join(", "))}</p>\n<pre>${esc(g.text)}</pre>`).join("\n")}
${missing.length ? `<h2>패키지에 라이선스 파일이 없는 구성 요소</h2>
<p>아래 구성 요소는 배포 패키지에 라이선스 파일이 들어 있지 않아, 표기된 SPDX 라이선스의 표준 전문(<a href="https://spdx.org/licenses/" target="_blank" rel="noreferrer">https://spdx.org/licenses/</a>)과 원 저장소의 고지를 따릅니다.</p>
<ul>${missing.map((p) => `<li>${esc(p.name)} ${esc(p.version)} — ${esc(p.license)} — ${link(p.repo)}</li>`).join("")}</ul>` : ""}
</body></html>
`;
mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, html);
console.log(`notices: ${pkgs.length} packages, ${groups.size} license texts, ${missing.length} without a license file → ${OUT}`);
