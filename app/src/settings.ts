// 설정 화면 — 접속·AI·MCP·편집기·정보 (main.ts 에서 나눔)

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errOf, type Profile, type Provider, type TnsEntry, type TnsView } from "./api";
import { DEFAULT_PREFS, STYLE_PRESETS, autoFormat, detectPrefs, formatSql, loadPrefs, savePrefs, setAutoFormat, type AutoFormat, type FormatPrefs } from "./format";
import { restoreEnabled, setRestoreEnabled } from "./tabsession";
import { DEFAULT_SNIPPETS, parse as parseSnippets, saveSnippets, snippets, type Snip } from "./snippets";
import { applyTheme, confirmBox, editorColorNow, field, h, modal, setEditorColor, themePref, toast, type EditorColorKey, type ThemePref } from "./ui";
import type { App } from "./main";

export async function settings(app: App, page = "접속") {
  const body = h("div", { class: "settings" });
  const nav = h("nav", { class: "settings-nav" });
  const pages: Record<string, () => Promise<HTMLElement>> = {
    "접속": () => profilesPage(app),
    "AI": () => providersPage(app),
    "MCP": () => mcpPage(app),
    "편집기": async () => editorPage(app),
    "정보": async () => aboutPage(app),
  };
  const content = h("div", { class: "settings-content" });
  const showPage = async (name: string) => {
    for (const b of nav.children) b.classList.toggle("active", b.textContent === name);
    content.replaceChildren(await pages[name]());
  };
  for (const name of Object.keys(pages)) nav.append(h("button", { onclick: (e: Event) => { e.preventDefault(); showPage(name); } }, name));
  body.append(nav, content);
  showPage(pages[page] ? page : "접속");
  await modal("설정", body, [{ label: "닫기", value: true, kind: "primary" }], { wide: true });
  await app.reloadProfiles();
  await app.ai.load();
}

/** 접속 진단 — 기록을 요약해 보여 주고 복사한다 (로그를 보내지 않고 말로 전할 수 있게) */
async function connReport(_app: App) {
  const days = h("select", {}, ...[[1, "오늘"], [7, "최근 7일"], [30, "최근 30일"]].map(([v, l]) => h("option", { value: String(v), selected: v === 7 }, String(l))));
  const mask = h("input", { type: "checkbox", checked: true }) as HTMLInputElement;
  const pre = h("pre", { class: "modal-text conn-report" });
  const tbody = h("tbody", {});
  const KIND: Record<string, string> = {
    connect_ok: "접속", connect_fail: "접속 실패", lost: "끊김", recover_ok: "다시 접속", recover_fail: "다시 접속 실패", meta_ok: "사전 조회 다시 접속", meta_fail: "사전 조회 다시 접속 실패",
  };
  const fill = async () => {
    const r = await invoke<{ summary: string; events: { t: number; kind: string; who?: string; target?: string; sid?: number; via?: string; idle?: number; ms?: number; code?: string; msg?: string }[]; path: string }>(
      "conn_report", { days: Number(days.value), mask: mask.checked });
    pre.textContent = r.summary;
    tbody.replaceChildren(...r.events.slice(0, 200).map((e) => h("tr", { class: e.kind.includes("fail") || e.kind === "lost" ? "warn" : "" },
      h("td", {}, new Date(e.t * 1000).toLocaleString()),
      h("td", {}, KIND[e.kind] ?? e.kind),
      h("td", {}, e.who ?? ""), h("td", {}, e.target ?? ""),
      h("td", {}, e.kind === "lost" ? `${e.via === "keepalive" ? "쉬는 동안" : "쓰다가"}${e.idle != null ? ` · ${e.idle >= 60 ? `${Math.round(e.idle / 60)}분` : `${e.idle}초`} 쉼` : ""}` : e.ms != null ? `${(e.ms / 1000).toFixed(1)}초` : ""),
      h("td", { title: e.msg ?? "" }, e.code ?? ""))));
  };
  days.onchange = () => void fill();
  mask.onchange = () => void fill();
  await fill().catch((e) => { pre.textContent = errOf(e).message; });
  const body = h("div", { class: "conn-report-box" },
    h("div", { class: "row" }, days, h("label", { class: "check", title: "주소·계정을 '접속1' 처럼 가리고, 오류 원문을 뺀다" }, mask, "주소·계정 가리기"),
      h("span", { class: "spacer" }),
      h("button", { class: "primary", onclick: () => navigator.clipboard.writeText(pre.textContent ?? "").then(() => toast("요약을 복사했습니다", "info", 1500)) }, "요약 복사")),
    pre,
    h("details", {}, h("summary", {}, "기록 (최근 것부터)"),
      h("div", { class: "conn-events" }, h("table", { class: "desc-table" },
        h("thead", {}, h("tr", {}, ...["때", "무엇", "세션", "접속", "상황", "오류"].map((x) => h("th", {}, x)))), tbody))));
  await modal("접속 진단", body, [{ label: "닫기", value: true }], { wide: true });
}

/** 데이터 폴더 지정 — 포터블을 버전마다 다른 폴더에 풀거나 설치판을 다시 깔아도 설정·이력을 같이 쓰게 (portable.txt / location.txt 의 data_dir=) */
async function dataDirRow(_app: App) {
  const d = await invoke<{ portable: boolean; fixed: boolean; dir: string }>("data_dir_info").catch(() => null);
  if (!d || d.fixed) return "";
  const apply = async (dir: string | null) => {
    try {
      const now = await invoke<string>("set_data_dir", { dir });
      if (await confirmBox("데이터 폴더", `${now}\n\n이 폴더를 씁니다. 고른 폴더에 설정이 없으면 지금 쓰던 설정·이력을 복사해 두었습니다.\n다음에 시작할 때부터 적용됩니다. 지금 다시 시작할까요?`, "다시 시작", false)) await invoke("restart_app");
    } catch (e) { toast(errOf(e).message, "error"); }
  };
  const def = d.portable ? "실행 파일 옆 data 폴더" : "%APPDATA%\\sqlstudio";
  return h("p", { class: "hint" }, `데이터 폴더${d.portable ? " (포터블)" : ""}: ${d.dir} `,
    h("button", { class: "small", title: `설정·실행 이력·분석 결과를 둘 폴더. 바깥 폴더(예: D:\\SQLStudio-data)를 정해 두면 새 버전을 다른 폴더에 풀거나 다시 설치해도 같은 폴더만 다시 고르면 됩니다. 이전 SQLStudio 포터블 폴더를 고르면 그 안의 data 를 씁니다`, onclick: async () => {
      const p = await openDialog({ directory: true, multiple: false, title: "데이터 폴더 (설정·이력을 둘 곳, 또는 이전 SQLStudio 폴더)", defaultPath: d.dir });
      if (p && !Array.isArray(p)) await apply(p);
    } }, "폴더 지정…"), " ",
    h("button", { class: "small", title: `기본 위치(${def})로 되돌린다`, onclick: () => void apply(null) }, "기본으로"));
}

async function aboutPage(app: App) {
  const info = await api.appInfo();
  return h("div", { class: "about" },
    h("p", {}, h("strong", {}, `SQLStudio ${info.version}`)),
    h("p", { class: "hint" }, `설정 파일: ${info.config_path}`),
    await dataDirRow(app),
    h("p", {}, h("button", { title: "접속·끊김·다시 접속 기록을 몇 줄로 요약 — 원인(쉬는 동안·실행 중·서버·한도)을 가른다", onclick: () => void connReport(app) }, "접속 진단")),
    info.log_dir ? h("p", { class: "hint" }, `로그: ${info.log_dir} `,
      h("button", { class: "small", title: "접속·끊김·다시 접속 기록 — 문제가 생기면 이 폴더의 sqlstudio.log 를 보내 주세요", onclick: () => invoke("open_log_dir").catch((e) => toast(errOf(e).message, "error")) }, "로그 폴더 열기")) : "",
    h("h4", {}, "오픈소스 라이선스"),
    h("p", { class: "hint" }, "이 프로그램에 포함된 오픈소스 구성 요소와 라이선스입니다. 설치 폴더의 THIRD_PARTY_NOTICES.html 과 같은 내용입니다."),
    h("iframe", { class: "about-notices", src: "/third-party-notices.html", title: "오픈소스 라이선스 고지" }));
}

function editorPage(app: App) {
  const p = loadPrefs();
  const sel = (cur: string, opts: [string, string][]) =>
    h("select", {}, ...opts.map(([v, l]) => h("option", { value: v, selected: v === cur }, l))) as HTMLSelectElement;
  const style = sel(p.style, Object.entries(STYLE_PRESETS).map(([k, v]) => [k, v.label]));
  const kw = sel(p.keywordCase, [["upper", "대문자 (SELECT)"], ["lower", "소문자 (select)"], ["preserve", "그대로"]]);
  const ident = sel(p.identCase, [["preserve", "그대로"], ["upper", "대문자 (EMP)"], ["lower", "소문자 (emp)"]]);
  const comma = sel(p.commaBefore ? "before" : "after", [["before", "줄 앞 (, SAL)"], ["after", "줄 끝 (SAL,)"]]);
  const ind = sel(String(p.indent), [["2", "2칸"], ["4", "4칸"]]);
  const logic = sel(p.logicalBefore ? "before" : "after", [["before", "줄 앞 (AND x = 1)"], ["after", "줄 끝 (x = 1 AND)"]]);
  const width = h("input", { type: "number", min: "20", max: "200", value: String(p.expressionWidth) }) as HTMLInputElement;
  const preview = h("pre", { class: "format-preview" });
  const sample = "select o.order_id, c.cust_nm, sum(i.qty * i.price) total, case when o.status = 'C' then '취소' else '정상' end st " +
    "from orders o join customers c on c.cust_id = o.cust_id left join order_items i on i.order_id = o.order_id and i.use_yn = 'Y' " +
    "where o.order_dt >= trunc(sysdate) - 7 and c.grade in ('A', 'B') and exists (select 1 from payments p where p.order_id = o.order_id) " +
    "group by o.order_id, c.cust_nm, o.status order by total desc";
  // 스타일마다 쓰는 항목만 보인다
  const fComma = field("쉼표 위치", comma);
  const fInd = field("들여쓰기", ind);
  const fWidth = field("한 줄 너비", width, " ");
  const read = (): FormatPrefs => ({
    style: style.value as FormatPrefs["style"],
    keywordCase: kw.value as FormatPrefs["keywordCase"],
    identCase: ident.value as FormatPrefs["identCase"],
    commaBefore: comma.value === "before",
    indent: ind.value === "4" ? 4 : 2,
    expressionWidth: Math.max(20, Math.min(200, Number(width.value) || DEFAULT_PREFS.expressionWidth)),
    logicalBefore: logic.value === "before",
  });
  const set = (q: Partial<FormatPrefs>) => {
    if (q.style) style.value = q.style;
    if (q.keywordCase) kw.value = q.keywordCase;
    if (q.identCase) ident.value = q.identCase;
    if (q.commaBefore !== undefined) comma.value = q.commaBefore ? "before" : "after";
    if (q.logicalBefore !== undefined) logic.value = q.logicalBefore ? "before" : "after";
    if (q.indent) ind.value = String(q.indent);
    if (q.expressionWidth) width.value = String(q.expressionWidth);
  };
  const update = () => {
    const prefs = read();
    savePrefs(prefs);
    fComma.hidden = prefs.style === "left";
    fInd.hidden = prefs.style !== "left";
    fWidth.hidden = prefs.style === "river";
    const hint = fWidth.querySelector("small");
    if (hint) hint.textContent = prefs.style === "left" ? "괄호 식이 이보다 길면 여러 줄로 나눕니다" : "목록을 이 너비까지 한 줄에 채웁니다";
    const r = formatSql(sample, prefs);
    preview.textContent = r.ok ? r.text : r.reason;
  };
  // 스타일을 고르면 그 스타일의 기본 모양으로 (대소문자는 그대로)
  style.addEventListener("change", () => { set(STYLE_PRESETS[style.value as FormatPrefs["style"]].prefs); update(); });
  for (const el of [kw, ident, comma, ind, logic, width]) el.addEventListener("change", update);
  const fromSample = h("button", { class: "small", title: "마음에 드는 모양으로 정리한 SQL 을 붙여 넣으면 쉼표·AND 위치, 키워드 정렬, 대소문자를 읽어 맞춘다", onclick: async (e: Event) => {
    e.preventDefault();
    const ta = h("textarea", { rows: 12, class: "snip-body", placeholder: "SELECT JOB\n     , MGR\n  FROM emp\n WHERE 1 = 1" }) as HTMLTextAreaElement;
    const ok = await modal("예시로 맞추기", h("div", { class: "form" },
      h("small", {}, "원하는 모양으로 정리한 SQL 을 붙여 넣으세요. 쉼표·AND 위치, 키워드 정렬, 키워드·이름 대소문자를 읽어 설정을 맞춥니다."), ta),
      [{ label: "취소", value: false }, { label: "맞추기", value: true, kind: "primary" }], { wide: true });
    if (!ok || !ta.value.trim()) return;
    const d = detectPrefs(ta.value);
    if (!d.found.length) return toast("읽을 수 있는 모양이 없습니다 — SELECT·FROM·쉼표가 든 SQL 을 넣어 보세요", "info");
    set(d.prefs);
    update();
    toast(`맞췄습니다: ${d.found.join(" · ")}`, "ok", 5000);
  } }, "예시로 맞추기…");
  update();
  const autoBox = h("div", { class: "auto-format" }, ...([
    ["caseOnType", "키워드 대소문자 바로 맞추기 (낱말을 다 치면)"],
    ["onSemicolon", "줄 끝에서 ; 를 치면 그 문장 정리"],
    ["onPaste", "SQL 문장을 붙여 넣으면 정리"],
    ["onRun", "실행(Ctrl+Enter)할 때 그 문장 정리"],
  ] as [keyof AutoFormat, string][]).map(([k, label]) => {
    const cb = h("input", { type: "checkbox", checked: autoFormat()[k] }) as HTMLInputElement;
    cb.onchange = () => setAutoFormat({ [k]: cb.checked });
    return h("label", { class: "check" }, cb, label);
  }));
  // 실행 이력 켜기/끄기·지우기
  const histOn = h("input", { type: "checkbox" });
  const histDir = h("small", {});
  const histBox = h("div", {},
    h("label", { class: "check" }, histOn, "실행한 SQL 을 이 PC 에 남기기 (바인드 값은 남기지 않음)"), histDir,
    h("div", { class: "row" }, h("button", { onclick: async (e: Event) => {
      e.preventDefault();
      if (!(await confirmBox("실행 이력 지우기", "남겨 둔 실행 이력을 모두 지웁니다. 되돌릴 수 없습니다.", "지우기"))) return;
      await invoke("history_clear").then(() => toast("지웠습니다", "ok")).catch((err) => toast(errOf(err).message, "error"));
    } }, "이력 모두 지우기")));
  invoke<{ enabled: boolean; keep_months: number; dir: string }>("history_settings").then((s) => {
    histOn.checked = s.enabled;
    histDir.textContent = `위치: ${s.dir} · 최근 ${s.keep_months}달만 남김`;
  }).catch(() => {});
  histOn.onchange = () => invoke("set_history_enabled", { enabled: histOn.checked }).catch((err) => toast(errOf(err).message, "error"));
  const theme = sel(themePref(), [["system", "시스템 따름 (Windows 설정)"], ["light", "밝게"], ["dark", "어둡게"]]);
  // 편집기 선택 색 — 밝게·어둡게마다 따로 기억한다 (테마를 바꾸면 그 테마의 색으로)
  const colorInputs: [EditorColorKey, HTMLInputElement][] = [];
  const colorRow = (key: EditorColorKey, label: string, hint: string) => {
    const input = h("input", { type: "color", style: "width:56px;height:28px;padding:0" }) as HTMLInputElement;
    input.value = editorColorNow(key);
    input.oninput = () => setEditorColor(key, input.value);
    colorInputs.push([key, input]);
    const reset = h("button", { class: "small", title: "이 테마의 기본 색으로", onclick: (e: Event) => { e.preventDefault(); setEditorColor(key, null); input.value = editorColorNow(key); } }, "기본값");
    return field(label, h("div", { class: "row", style: "display:flex;gap:8px;align-items:center" }, input, reset), hint);
  };
  theme.onchange = () => { applyTheme(theme.value as ThemePref); setTimeout(() => { for (const [k, i] of colorInputs) i.value = editorColorNow(k); }, 0); };
  return h("div", { class: "form" },
    h("h3", {}, "화면"),
    field("화면 테마", theme, "바꾸면 바로 적용되고 이 PC 에 기억합니다. 흐름도 창도 따라갑니다"),
    colorRow("sel", "편집기 선택 영역 색", "글을 드래그하거나 낱말을 더블클릭해 고른 곳의 배경색 — 밝은 화면·어두운 화면이 따로 기억됩니다"),
    colorRow("match", "같은 낱말 강조 색", "낱말을 고르면 같은 낱말이 나오는 다른 곳에 칠하는 배경색"),
    h("h3", {}, "SQL 포맷 (Ctrl+Shift+F)"),
    field("스타일", h("div", { class: "row", style: "display:flex;gap:8px;align-items:center" }, style, fromSample)),
    field("쓰는 동안 자동 포맷", autoBox, "파일 메뉴 > 자동 포맷 에서도 켜고 끕니다"),
    field("키워드", kw), field("테이블·컬럼 이름", ident), fComma, field("AND / OR 위치", logic), fInd, fWidth,
    h("small", {}, "바꾸면 바로 저장됩니다. 포맷 결과가 공백·대소문자 말고 원본과 달라지면 적용하지 않습니다. PL/SQL 블록은 아직 포맷하지 않습니다. " +
      "오른쪽 맞춤·간결은 SELECT·WITH·INSERT·UPDATE·DELETE 를 다루고, 그 밖(MERGE·DDL)은 왼쪽 정렬로 정리합니다."),
    field("미리 보기", preview),
    h("h3", {}, "여러 곳 편집 (열 편집)"),
    h("pre", { class: "format-preview" },
      "Alt + 드래그          사각형(열) 선택\n" +
      "Alt + Shift + 방향키   키보드로 사각형 선택\n" +
      "Ctrl + 클릭           커서 추가\n" +
      "Ctrl + D              같은 단어 하나 더 선택\n" +
      "Esc                   커서 하나로"),
    h("h3", {}, "탭"),
    h("label", { class: "check" }, (() => {
      const cb = h("input", { type: "checkbox", checked: restoreEnabled() }) as HTMLInputElement;
      cb.onchange = () => setRestoreEnabled(cb.checked);
      return cb;
    })(), "시작할 때 지난 탭 되살리기 — 쓰던 SQL 을 몇 초마다 데이터 폴더(tabs.json)에 남깁니다. 끄면 닫을 때 저장 여부를 묻습니다"),
    h("h3", {}, "SQL 실행 이력 (F8)"),
    histBox,
    h("h3", {}, "코드 조각"),
    snippetEditor(app),
  );
}

/** 코드 조각 목록·편집·가져오기/내보내기 */
function snippetEditor(_app: App) {
  let items: Snip[] = snippets().map((s) => ({ ...s }));
  let cur = -1;
  const listEl = h("div", { class: "snip-list" });
  const key = h("input", { placeholder: "줄임말 (예: sel)", style: "width:120px" });
  const desc = h("input", { placeholder: "설명", style: "flex:1" });
  const body = h("textarea", { rows: 7, class: "snip-body", placeholder: "SELECT ${*}\n  FROM ${table}" });
  const persist = async (next: Snip[], msg = "저장했습니다") => {
    try { await saveSnippets(next); items = next.map((s) => ({ ...s })); render(); toast(msg, "ok"); }
    catch (err) { toast(errOf(err).message, "error"); }
  };
  const render = () => {
    listEl.replaceChildren(...items.map((s, i) => {
      const b = h("button", { class: `list-item${i === cur ? " active" : ""}`, onclick: (e: Event) => { e.preventDefault(); pick(i); } },
        h("code", {}, s.key), h("small", {}, ` ${s.desc}`));
      return b;
    }));
  };
  const pick = (i: number) => {
    cur = i;
    const s = items[i];
    key.value = s?.key ?? ""; desc.value = s?.desc ?? ""; body.value = s?.body ?? "";
    render();
  };
  const btn = (label: string, f: () => void, cls = "") => h("button", { class: cls, onclick: (e: Event) => { e.preventDefault(); f(); } }, label);
  const box = h("div", { class: "snip" },
    h("small", {}, "편집기에서 줄임말을 치면 자동완성에 뜹니다. 고르면 틀이 들어가고 ${이름} 칸을 Tab 으로 옮겨 다니며 채웁니다 (이름이 같은 칸은 함께 바뀜)."),
    h("div", { class: "snip-main" }, listEl,
      h("div", { class: "snip-form" },
        h("div", { class: "row" }, key, desc), body,
        h("div", { class: "row" },
          btn("저장", () => {
            const k = key.value.trim();
            if (!k || !body.value.trim()) return toast("줄임말과 내용을 채우세요", "error");
            const next = items.slice();
            const dup = next.findIndex((s, i) => i !== cur && s.key.toLowerCase() === k.toLowerCase());
            if (dup >= 0) return toast(`'${k}' 는 이미 있습니다`, "error");
            const s = { key: k, desc: desc.value.trim(), body: body.value };
            if (cur >= 0) next[cur] = s; else { next.push(s); cur = next.length - 1; }
            persist(next);
          }, "primary"),
          btn("새 조각", () => pick(-1)),
          btn("삭제", async () => {
            if (cur < 0) return;
            if (!(await confirmBox("조각 삭제", `'${items[cur].key}' 를 지울까요?`, "삭제"))) return;
            const next = items.filter((_, i) => i !== cur); cur = -1; key.value = desc.value = body.value = "";
            persist(next, "지웠습니다");
          }, "danger")))),
    h("div", { class: "row" },
      btn("내보내기", async () => {
        const path = await saveDialog({ filters: [{ name: "JSON", extensions: ["json"] }], defaultPath: "sqlstudio-snippets.json" });
        if (!path) return;
        await api.writeFile(path, JSON.stringify({ snippets: items }, null, 2), "utf-8").then(() => toast("내보냈습니다 — 팀원은 '가져오기'로 씁니다", "ok")).catch((err) => toast(errOf(err).message, "error"));
      }),
      btn("가져오기", async () => {
        const path = await openDialog({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
        if (!path || Array.isArray(path)) return;
        try {
          const got = parseSnippets((await api.readFile(path)).text);
          // 같은 줄임말은 가져온 것으로 바꾸고 나머지는 더한다
          const next = items.filter((s) => !got.some((g) => g.key.toLowerCase() === s.key.toLowerCase())).concat(got);
          persist(next, `${got.length}개 가져왔습니다`);
        } catch (err) { toast(`가져오지 못했습니다: ${errOf(err).message}`, "error"); }
      }),
      btn("기본 조각으로", async () => {
        if (!(await confirmBox("기본 조각으로", "지금 조각을 지우고 기본 조각으로 되돌립니다.", "되돌리기"))) return;
        cur = -1; persist(DEFAULT_SNIPPETS.map((s) => ({ ...s })), "기본 조각으로 되돌렸습니다");
      })));
  render();
  return box;
}

async function profilesPage(app: App) {
  const list = h("div", { class: "list" });
  const form = h("div", { class: "form" });
  // tnsnames.ora 별칭 — 접속 문자열 칸에서 고른다
  const tns = await api.tnsList().catch(() => ({ entries: [] } as TnsView));
  const tnsList = h("datalist", { id: "tns-aliases" }, ...tns.entries.map((e) => h("option", { value: e.alias }, [e.host, e.port].filter(Boolean).join(":") + (e.service ? `/${e.service}` : ""))));
  const where = (e: TnsEntry) => `${e.host ?? "?"}${e.port ? `:${e.port}` : ""}${e.service ? `/${e.service}` : ""}`;
  const edit = (p?: Profile) => {
    const name = h("input", { value: p?.name ?? "" });
    const user = h("input", { value: p?.user ?? "" });
    const cs = h("input", { value: p?.connect_string ?? "", placeholder: tns.entries.length ? `TNS 별칭 (${tns.entries.length}개에서 고르기) 또는 host:1521/SERVICE` : "host:1521/SERVICE 또는 TNS 별칭" });
    cs.setAttribute("list", "tns-aliases");
    const csHint = h("small", { class: "tns-hint" });
    const showHint = () => {
      const e = tns.entries.find((x) => x.alias === cs.value.trim().toUpperCase());
      csHint.textContent = e ? `TNS ${e.alias} → ${where(e)}` : "";
    };
    cs.addEventListener("input", () => {
      showHint();
      // 새 접속에서 별칭을 고르면 이름도 채운다
      const e = tns.entries.find((x) => x.alias === cs.value.trim().toUpperCase());
      if (e && !p && !name.value.trim()) name.value = e.alias;
    });
    showHint();
    const ro = h("input", { type: "checkbox", checked: !!p?.read_only });
    const prod = h("input", { type: "checkbox", checked: !!p?.production });
    const color = h("input", { type: "color", value: p?.color ?? "#3a7bd5" });
    const useColor = h("input", { type: "checkbox", checked: !!p?.color });
    const timeout = h("input", { type: "number", min: "0", value: p?.call_timeout_secs ?? "" , placeholder: "없음" });
    form.replaceChildren(
      field("이름", name), field("사용자", user), field("접속 문자열", cs, "11g 는 Instant Client 가 필요합니다 (19c 권장)"), csHint,
      h("label", { class: "check" }, ro, "읽기 전용 (SELECT 만, 서버에서도 막음)"),
      h("label", { class: "check", title: "INSERT·UPDATE·DELETE·DDL·PL/SQL 블록·COMMIT 을 실행 전에 항상 확인합니다" }, prod, "운영 DB (바꾸는 문장·커밋 전에 항상 확인)"),
      h("label", { class: "check" }, useColor, "탭 색 표시 (운영 DB 는 빨강 권장)", color),
      field("호출 시간 상한(초)", timeout, "Oracle Client 18 이상에서만 동작"),
      h("small", {}, "비밀번호는 설정 파일에 쓰지 않습니다. 접속할 때 묻거나(‘비밀번호 저장’ 을 고르면 OS 자격 증명 관리자에), 환경변수 SQLSTUDIO_PW_<이름> 을 씁니다."),
      p?.has_saved_password ? h("div", { class: "row" }, h("span", {}, "저장된 비밀번호가 있습니다"),
        h("button", { class: "small", onclick: async (e: Event) => {
          e.preventDefault();
          await api.forgetPassword(p.name).catch((err) => toast(errOf(err).message, "error"));
          toast("저장된 비밀번호를 지웠습니다", "ok");
          await refreshList();
          (e.target as HTMLElement).parentElement?.remove();
        } }, "지우기")) : "",
      h("div", { class: "row" },
        h("button", { class: "primary", onclick: async (e: Event) => {
          e.preventDefault();
          try {
            await api.saveProfile({
              name: name.value.trim(), user: user.value.trim(), connect_string: cs.value.trim(),
              read_only: ro.checked, production: prod.checked, color: useColor.checked ? color.value : null,
              call_timeout_secs: timeout.value ? Number(timeout.value) : null,
            }, p?.name);
            toast("저장했습니다", "ok");
            await refreshList();
          } catch (err) { toast(errOf(err).message, "error"); }
        } }, "저장"),
        p ? h("button", { class: "danger", onclick: async (e: Event) => {
          e.preventDefault();
          if (!(await confirmBox("삭제", `${p.name} 프로필을 지울까요?`, "삭제"))) return;
          await api.deleteProfile(p.name).catch((err) => toast(errOf(err).message, "error"));
          form.replaceChildren();
          await refreshList();
        } }, "삭제") : null));
  };
  const refreshList = async () => {
    const ps = await api.listProfiles();
    list.replaceChildren(
      ...ps.map((p) => h("button", { class: "list-item", onclick: (e: Event) => { e.preventDefault(); edit(p); } },
        h("span", { class: "dot", style: `background:${p.color ?? "transparent"}` }), `${p.name}`, h("small", {}, ` ${p.user}@${p.connect_string}`))),
      h("button", { class: "list-item add", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 접속"));
  };
  await refreshList();
  if (!app.profiles.length) edit();
  const pickTns = async (e: Event) => {
    e.preventDefault();
    const d = await openDialog({ directory: true, title: "tnsnames.ora 가 있는 폴더" });
    if (!d || Array.isArray(d)) return;
    try {
      await api.setTnsAdmin(d);
      toast("저장했습니다 — 프로그램을 다시 시작하면 이 폴더의 별칭으로 접속합니다", "ok", 6000);
    } catch (err) { toast(errOf(err).message, "error"); }
  };
  const tnsBox = h("div", { class: "tns-box" },
    h("strong", {}, "tnsnames.ora "),
    tns.dir ? h("span", { title: tns.configured && tns.configured !== tns.dir ? `설정: ${tns.configured} (프로그램 폴더 기준 상대 경로)` : tns.dir },
      `${tns.dir} · 별칭 ${tns.entries.length}개${tns.configured ? (tns.configured !== tns.dir ? ` (설정: ${tns.configured})` : "") : " (자동으로 찾음)"}`)
      : h("span", { class: "warn" }, "찾지 못했습니다 — TNS_ADMIN·ORACLE_HOME·Instant Client 폴더를 봤습니다"),
    tns.error ? h("span", { class: "warn" }, ` · 읽기 실패: ${tns.error}`) : "",
    h("button", { class: "small", onclick: pickTns }, "폴더 지정"),
    tns.configured ? h("button", { class: "small", onclick: async (e: Event) => { e.preventDefault(); await api.setTnsAdmin(null); toast("지정을 지웠습니다 — 다시 시작하면 기본 위치를 씁니다", "ok"); } }, "지정 해제") : "");
  return h("div", {}, tnsBox, tnsList, h("div", { class: "split" }, list, form));
}

/** 문법 안내 카드 보기 — 실제로 AI 에 보내는 내용. 기본 카드는 회사 규칙 파일로 옮겨 고친다 */
async function showGuides(app: App) {
  type Card = { id: string; title: string; keys: string[]; version: string; body: string; source: "builtin" | "custom" | "override" };
  const cards = await invoke<Card[]>("ai_guides_list").catch((e) => { toast(errOf(e).message, "error"); return null; });
  if (!cards) return;
  const SRC = { builtin: ["기본", ""], custom: ["회사 규칙", "ok"], override: ["회사 규칙이 바꿈", "warn"] } as const;
  const q = h("input", { placeholder: "찾기 (제목·낱말·본문)", class: "sc-q" }) as HTMLInputElement;
  const list = h("div", { class: "guide-list" });
  const edit = async (id: string) => {
    try {
      const path = await invoke<string>("ai_guides_customize", { id });
      list.closest("dialog")?.close();
      await app.openPath(path);
      toast(`'${id}' 카드를 회사 규칙 파일에 두었습니다 — 고치고 저장하면 다음 질문부터 씁니다`, "info", 6000);
    } catch (e) { toast(errOf(e).message, "error"); }
  };
  const render = () => {
    const w = q.value.trim().toLowerCase();
    list.replaceChildren(...cards.filter((c) => !w || `${c.id} ${c.title} ${c.keys.join(" ")} ${c.body}`.toLowerCase().includes(w)).map((c) =>
      h("details", { class: "guide" },
        h("summary", {}, h("strong", {}, c.title), h("small", { class: "dim" }, ` ${c.id}`),
          h("span", { class: `badge ${SRC[c.source][1]}` }, SRC[c.source][0]),
          c.version ? h("span", { class: "badge" }, c.version.startsWith("-") ? `${c.version.slice(1)} 이하` : c.version.endsWith("-") ? `${c.version.slice(0, -1)} 이상` : c.version) : ""),
        h("div", { class: "hint" }, c.keys.includes("*") ? "항상 넣는다 (SQL 만들기·고치기·대화)" : `이 낱말이 요청에 있으면: ${c.keys.join(", ")}`),
        h("pre", { class: "guide-body" }, c.body),
        h("button", { class: "small", onclick: () => void edit(c.id) }, c.source === "builtin" ? "회사 규칙으로 옮겨 고치기" : "회사 규칙 파일에서 고치기"))));
  };
  q.addEventListener("input", render);
  render();
  const n = (s: Card["source"]) => cards.filter((c) => c.source === s).length;
  await modal("Oracle 문법 안내 카드", h("div", { class: "guides-view" },
    h("p", { class: "hint" }, `요청에 맞는 카드를 몇 장 골라 AI 에 함께 보냅니다. 기본 ${n("builtin") + n("override")}장 (회사 규칙이 바꾼 것 ${n("override")}) · 회사 규칙만 ${n("custom")}장. ` +
      "기본 카드를 고치면 회사 규칙 파일에 같은 아이디로 옮겨 두고 그것을 씁니다 — 지우면 기본으로 돌아갑니다."),
    q, list), [{ label: "닫기", value: true, kind: "primary" }], { wide: true });
}

async function providersPage(app: App) {
  const list = h("div", { class: "list" });
  const form = h("div", { class: "form" });
  const edit = (p?: Provider) => {
    const name = h("input", { value: p?.name ?? "" });
    const kind = h("select", {},
      h("option", { value: "ollama", selected: p?.kind === "ollama" }, "Ollama (로컬)"),
      h("option", { value: "openai_compatible", selected: p?.kind === "openai_compatible" }, "OpenAI 호환 (LM Studio, vLLM, OpenAI…)"),
      h("option", { value: "anthropic", selected: p?.kind === "anthropic" }, "Anthropic (Claude)"));
    const base = h("input", { value: p?.base_url ?? "", placeholder: "비우면 기본값" });
    const model = h("input", { value: p?.model ?? "", style: "flex:1" });
    // 서버에서 받아 온 모델 목록 — 골라서 모델 칸에 넣는다 (직접 쳐도 된다)
    const modelPick = h("select", { title: "서버에서 받아 온 모델 — 고르면 모델 칸에 들어갑니다", hidden: true, style: "max-width:100%" }) as HTMLSelectElement;
    const modelNote = h("span", { class: "hint" });
    const keyEnv = h("input", { value: p?.api_key_env ?? "", placeholder: "예: ANTHROPIC_API_KEY" });
    const key = h("input", { type: "password", placeholder: p?.has_key ? "(설정됨)" : "API 키" });
    const keyRemember = h("input", { type: "checkbox" });
    const numCtx = h("input", { type: "number", value: p?.num_ctx ?? "", placeholder: "16384" });
    const effort = h("select", {}, ...["", "low", "medium", "high", "xhigh", "max"].map((x) => h("option", { value: x, selected: (p?.effort ?? "") === x }, x || "(기본)")));
    // 생각(thinking) — Ollama 는 끄기·켜기·수준, OpenAI 호환은 끄기·수준(reasoning_effort)
    const thinkOpts: [string, string][] = [["", "(모델 기본)"], ["off", "끄기 — 생각 없이 바로 답 (빠름)"], ["on", "켜기"], ["low", "낮음 (low)"], ["medium", "중간 (medium)"], ["high", "높음 (high)"]];
    const think = h("select", {}, ...thinkOpts.map(([v, l]) => h("option", { value: v, selected: (p?.think ?? "") === v }, l))) as HTMLSelectElement;
    const fallbacks = h("input", { type: "checkbox", checked: p?.fallbacks ?? true });
    const maxTokens = h("input", { type: "number", value: p?.max_tokens ?? 8192 });
    // 사내 PC 는 시스템 프록시가 잡혀 있어 사내 LLM 서버로 가는 요청까지 프록시로 나가 막히곤 한다 (브라우저는 PAC 로 직접 연결)
    const proxy = h("select", {},
      h("option", { value: "", selected: !p?.proxy || p.proxy === "auto" }, "자동 — 사내 주소(사설 IP·점 없는 이름·.local/.corp 등)는 직접, 그 밖은 시스템 프록시"),
      h("option", { value: "none", selected: p?.proxy === "none" }, "사용 안 함 — 항상 직접 연결"),
      h("option", { value: "system", selected: p?.proxy === "system" }, "시스템 프록시 사용"));
    const result = h("pre", { class: "modal-text" });
    const collect = (): Provider => ({
      name: name.value.trim(), kind: kind.value as Provider["kind"], base_url: base.value.trim() || null,
      model: model.value.trim(), api_key_env: keyEnv.value.trim() || null, num_ctx: numCtx.value ? Number(numCtx.value) : null,
      effort: effort.value || null, fallbacks: fallbacks.checked, max_tokens: Number(maxTokens.value) || 8192,
      proxy: proxy.value || null, think: kind.value === "anthropic" ? null : think.value || null,
    });
    // 모델 목록 받아 오기 — 저장하지 않은 주소로도 된다. 종류가 Ollama 면 /api/tags
    let listRun = 0;
    const loadModels = async (quiet: boolean) => {
      const run = ++listRun;
      modelNote.textContent = "모델 목록을 받는 중…";
      try {
        const names = await api.listModels({ ...collect(), name: name.value.trim() || p?.name || "새 공급자", model: model.value.trim() || "x" });
        if (run !== listRun) return;
        modelPick.replaceChildren(h("option", { value: "" }, `— 서버의 모델 ${names.length}개 중 고르기 —`),
          ...names.map((n) => h("option", { value: n, selected: n === model.value.trim() }, n)));
        modelPick.hidden = !names.length;
        const has = !model.value.trim() || names.some((n) => n === model.value.trim() || n.replace(/:latest$/, "") === model.value.trim());
        modelNote.textContent = names.length ? `${names.length}개${has ? "" : ` — 설정한 '${model.value.trim()}' 은 이 서버에 없습니다`}` : "모델이 없습니다 (서버에서 ollama pull 로 받으세요)";
      } catch (err) {
        if (run !== listRun) return;
        modelPick.hidden = true;
        modelNote.textContent = quiet ? "" : `목록을 받지 못했습니다: ${errOf(err).message}`;
      }
    };
    modelPick.onchange = () => { if (modelPick.value) model.value = modelPick.value; };
    const modelBtn = h("button", { class: "small", title: "서버에서 모델 목록을 받아 옵니다 (Ollama: /api/tags)", onclick: (e: Event) => { e.preventDefault(); void loadModels(false); } }, "목록 받기");
    // 종류에 맞는 칸만
    const thinkField = field("Thinking (생각)", think, "Ollama: 끄기 = think:false, low/medium/high 는 gpt-oss 같은 모델만. OpenAI 호환: reasoning_effort (끄기 = none). 모델이 지원하지 않으면 서버가 오류를 냅니다");
    const effortField = field("effort (Claude)", effort);
    const ctxField = field("num_ctx (Ollama)", numCtx, "기본 컨텍스트(2~4K)는 스키마 문맥을 조용히 잘라 버립니다");
    const syncKind = () => {
      const k = kind.value;
      thinkField.hidden = k === "anthropic";
      effortField.hidden = k !== "anthropic";
      ctxField.hidden = k !== "ollama";
    };
    kind.onchange = () => { syncKind(); modelPick.hidden = true; modelNote.textContent = ""; if (kind.value === "ollama") void loadModels(true); };
    let urlT = 0;
    base.addEventListener("input", () => { clearTimeout(urlT); urlT = window.setTimeout(() => { if (kind.value === "ollama") void loadModels(true); }, 600); });
    const persist = async () => {
      const np = collect();
      await api.saveProvider(np, p?.name);
      if (key.value) await api.setApiKey(np.name, key.value, keyRemember.checked);
      return np;
    };
    form.replaceChildren(
      field("이름", name), field("종류", kind), field("주소", base, "Ollama: http://127.0.0.1:11434 · 사내 GPU 서버 주소도 됩니다 (http:// 를 빼도 됩니다)"),
      field("프록시", proxy, "브라우저에서는 되는데 여기서 안 되면 '사용 안 함'으로"),
      field("모델", h("div", { style: "display:flex;flex-direction:column;gap:4px" },
        h("div", { style: "display:flex;gap:6px" }, model, modelBtn), modelPick, modelNote), "직접 쳐도 되고, '목록 받기'로 서버의 모델을 골라도 됩니다 (예: qwen2.5-coder:14b, gemma3:12b)"),
      field("API 키 환경변수", keyEnv), field("API 키", key, "설정 파일에는 쓰지 않습니다"),
      h("label", { class: "check" }, keyRemember, "OS 자격 증명 관리자에 저장 (끄면 이 실행 동안만)"),
      ctxField, thinkField, effortField,
      h("label", { class: "check" }, fallbacks, "거절 시 대체 모델로 이어서 답하기 (Claude API 직통일 때만)"),
      field("max_tokens", maxTokens),
      h("div", { class: "row" },
        h("button", { class: "primary", onclick: async (e: Event) => {
          e.preventDefault();
          try { await persist(); toast("저장했습니다", "ok"); await refreshList(); } catch (err) { toast(errOf(err).message, "error"); }
        } }, "저장"),
        h("button", { onclick: async (e: Event) => {
          e.preventDefault();
          try {
            const np = await persist();
            result.textContent = "연결 확인 중…";
            const models = await api.testProvider(np.name);
            // 서버에는 붙었어도 설정한 모델이 없으면 질문이 "모델 없음"으로 실패한다
            const has = !np.model || models.some((m) => m === np.model || m.replace(/:latest$/, "") === np.model);
            result.textContent = (has ? `연결됨 · 모델 ${models.length}개`
              : `연결됨 — 그런데 설정한 모델 '${np.model}' 이 이 서버에 없습니다. 아래 이름 중 하나로 바꾸세요 (또는 서버에서 ollama pull ${np.model}).`) +
              `\n${models.slice(0, 30).join("\n")}`;
            result.classList.toggle("warn", !has);
          } catch (err) { result.textContent = errOf(err).message; }
        } }, "연결 테스트"),
        p ? h("button", { class: "danger", onclick: async (e: Event) => {
          e.preventDefault();
          if (!(await confirmBox("AI 연결 삭제", `'${p.name}' 연결을 지울까요?\n저장해 둔 API 키(OS 자격 증명 관리자)도 함께 지웁니다.`, "삭제"))) return;
          try {
            await api.deleteProvider(p.name);
            form.replaceChildren();
            toast("지웠습니다", "ok");
            await refreshList();
          } catch (err) { toast(errOf(err).message, "error"); }
        } }, "삭제") : null),
      result);
    syncKind();
    if (kind.value === "ollama") void loadModels(true);
  };
  const refreshList = async () => {
    const ps = await api.listProviders();
    list.replaceChildren(
      ...ps.map((p) => h("button", { class: "list-item", onclick: (e: Event) => { e.preventDefault(); edit(p); } },
        `${p.name}`, h("small", {}, ` ${p.model}${p.remote ? " · 외부" : " · 로컬"}`))),
      h("button", { class: "list-item add", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 공급자"));
  };
  await refreshList();
  // Oracle 문법 안내 — 요청에 맞는 카드를 골라 넣는다. 회사 규칙은 사용자 파일에
  const gi = await invoke<{ path: string; exists: boolean; builtin: number; custom: number }>("ai_guides_info").catch(() => null);
  const guideSel = h("select", {},
    h("option", { value: "auto" }, "자동 (낱말로 약할 때만)"),
    h("option", { value: "always" }, "항상"),
    h("option", { value: "off" }, "끔 (낱말로만)")) as HTMLSelectElement;
  const guides = h("div", { class: "guides-box" },
    h("strong", {}, "Oracle 문법 안내 "),
    h("span", { class: "hint" }, `질문·SQL·오류에 맞는 안내 카드를 몇 장 골라 AI 에 함께 보냅니다 (기본 ${gi?.builtin ?? "?"}장${gi?.custom ? ` + 회사 규칙 ${gi.custom}장` : ""}). AI 답 위에 '참고 문법'으로 표시됩니다.`),
    h("button", { class: "small", title: gi?.path ?? "", onclick: async (e: Event) => {
      e.preventDefault();
      try {
        const path = await invoke<string>("ai_guides_edit");
        await app.openPath(path);
        toast("편집하고 저장하면 다음 질문부터 씁니다", "info", 5000);
      } catch (err) { toast(errOf(err).message, "error"); }
    } }, gi?.exists ? "회사 규칙 편집" : "회사 규칙 추가"),
    h("button", { class: "small", title: "AI 에 보내는 문법 안내 카드를 모두 보고, 기본 카드를 회사 규칙으로 옮겨 고친다", onclick: (e: Event) => { e.preventDefault(); void showGuides(app); } }, "카드 보기"),
    h("label", { class: "check", title: "카드 주제 목록을 AI 에 먼저 보내 고르게 한 뒤, 고른 카드만 질문과 함께 보낸다 (질의가 한 번 더 — 많아야 10초, 실패하면 낱말로 고른 것만)" },
      "AI 로도 고르기 ", guideSel));
  void invoke<string>("ai_guide_select").then((v) => { guideSel.value = v; }).catch(() => {});
  guideSel.addEventListener("change", () => {
    invoke("set_ai_guide_select", { mode: guideSel.value }).then(() => toast("저장했습니다", "ok", 1200)).catch((e) => toast(errOf(e).message, "error"));
  });
  return h("div", {}, guides, h("div", { class: "split" }, list, form));
}

async function mcpPage(_app: App) {
  const [s, profiles] = await Promise.all([api.getMcp(), api.listProfiles()]);
  const checks = profiles.map((p) => ({ p, c: h("input", { type: "checkbox", checked: s.allowed_connections.some((a) => a.toUpperCase() === p.name.toUpperCase()) }) }));
  const explain = h("input", { type: "checkbox", checked: s.allow_explain });
  const timeout = h("input", { type: "number", value: s.call_timeout_secs });
  const snippet = h("textarea", { class: "snippet", rows: 12, readOnly: true });
  const refreshSnippet = async () => { snippet.value = await api.mcpSnippet(); };
  await refreshSnippet();
  return h("div", { class: "form" },
    h("p", {}, "AI 에이전트(Claude Desktop, Claude Code, Cursor 등)가 sqlstudio-mcp 로 체크한 접속의 ",
      h("strong", {}, "스키마 정보(객체 목록·테이블 구조·DDL)만"), " 읽습니다. ",
      h("strong", {}, "AI 는 SQL 을 실행할 수 없고 테이블 데이터도 받지 못합니다."), " AI 가 만든 SQL 은 사람이 에디터에서 확인하고 실행합니다."),
    ...checks.map(({ p, c }) => h("label", { class: "check" }, c, `${p.name} (${p.user}@${p.connect_string})`) as Node),
    field("사전 조회 시간 상한(초)", timeout),
    h("label", { class: "check" }, explain, "explain_plan 허용 — AI 가 쓴 SQL 을 파서에 넘겨 실행계획만 봅니다 (실행·데이터 반환 없음)"),
    h("div", { class: "row" }, h("button", { class: "primary", onclick: async (e: Event) => {
      e.preventDefault();
      try {
        await api.setMcp({ allowed_connections: checks.filter((x) => x.c.checked).map((x) => x.p.name), call_timeout_secs: Number(timeout.value), allow_explain: explain.checked });
        await refreshSnippet();
        toast("저장했습니다", "ok");
      } catch (err) { toast(errOf(err).message, "error"); }
    } }, "저장"),
    h("button", { onclick: (e: Event) => { e.preventDefault(); navigator.clipboard.writeText(snippet.value).then(() => toast("복사했습니다", "ok")); } }, "설정 복사")),
    field("MCP 호스트 설정 (claude_desktop_config.json 등에 붙여 넣기)", snippet, "<비밀번호> 를 실제 값으로 바꾸세요"));
}
