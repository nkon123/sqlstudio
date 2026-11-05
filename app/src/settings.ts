// 설정 화면 — 접속·AI·MCP·편집기·정보 (main.ts 에서 나눔)

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errOf, type AcpAgent, type Group, type ModelProfile, type Profile, type Provider, type Task, type TnsEntry, type TnsView } from "./api";
import { colorOf, countIn, groupLabel, groupLeaf, groupParent, groupTree, isCollapsed, setCollapsed, type GroupNode } from "./conngroups";
import { DEFAULT_PREFS, STYLE_PRESETS, autoFormat, detectPrefs, formatSql, loadPrefs, savePrefs, setAutoFormat, type AutoFormat, type FormatPrefs } from "./format";
import { restoreEnabled, setRestoreEnabled } from "./tabsession";
import { revealEnabled, setRevealEnabled } from "./tab";
import { setTabsSidePref, tabsSidePref } from "./tabdrag";
import { DEFAULT_SNIPPETS, parse as parseSnippets, saveSnippets, snippets, type Snip } from "./snippets";
import { applyTheme, confirmBox, editorColorNow, field, h, modal, setEditorColor, themePref, toast, type EditorColorKey, type ThemePref } from "./ui";
import type { App } from "./main";
import { icon } from "./ui";

/** 설정 페이지 — 왼쪽 목록의 이름·아이콘, 위의 한 줄 설명 */
const PAGES: { name: string; icon: string; desc: string; make: (app: App) => Promise<HTMLElement> }[] = [
  { name: "접속", icon: "M7 6v4M17 6v4M5 10h14v3a7 7 0 0 1-14 0zM12 20v2", desc: "DB 접속 프로필과 tnsnames.ora", make: (app) => profilesPage(app) },
  { name: "AI", icon: "M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8zM19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8z", desc: "AI 공급자·모델과 ACP 에이전트", make: (app) => providersPage(app) },
  { name: "하네스", icon: "M4 6h6v5H4zM14 6h6v5h-6zM4 15h6v4H4zM14 15h6v4h-6zM10 8.5h4M7 11v4M17 11v4", desc: "AI 질문 하나가 거치는 단계(문맥 모으기 → 보내기 → 답 확인)와 그 설정", make: (app) => harnessPage(app) },
  { name: "MCP", icon: "M9 7H6a4 4 0 0 0 0 8h3M15 7h3a4 4 0 0 1 0 8h-3M8 11h8", desc: "Claude Desktop·Cursor 같은 AI 도구가 이 앱의 접속(스키마 정보)을 쓰게 하는 설정", make: (app) => mcpPage(app) },
  { name: "편집기", icon: "M8 7l-5 5 5 5M16 7l5 5-5 5M14 4l-4 16", desc: "화면 테마, SQL 포맷, 편집 동작, 실행 이력, 코드 조각", make: async (app) => editorPage(app) },
  { name: "정보", icon: "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM12 16v-5M12 8h.01", desc: "버전, 데이터 폴더, 로그, 오픈소스 라이선스", make: async (app) => aboutPage(app) },
];

/** 목록에서 지금 고친 항목을 표시 (data-name 으로) */
function markActive(list: HTMLElement, name: string | undefined) {
  for (const b of list.querySelectorAll<HTMLElement>("[data-name]")) b.classList.toggle("active", !!name && b.dataset.name === name);
}


/** 페이지를 카드로 — 소제목(h3·h4)마다 한 장, 소제목이 없으면 통째로 한 장 */
function cards(page: HTMLElement): HTMLElement {
  const out = h("div", { class: "set-cards" });
  let card: HTMLElement | null = null;
  for (const k of [...page.children] as HTMLElement[]) {
    if (k.tagName === "H3" || k.tagName === "H4") {
      card = h("section", { class: "set-card" }, h("h4", { class: "set-card-title" }, k.textContent ?? ""));
      out.append(card);
      continue;
    }
    if (!card) out.append((card = h("section", { class: "set-card" })));
    card.append(k);
  }
  return out;
}

export async function settings(app: App, page = "접속") {
  const nav = h("nav", { class: "settings-nav" }, h("div", { class: "settings-brand" }, "설정"));
  const title = h("h2", {});
  const desc = h("p", {});
  const content = h("div", { class: "settings-content" });
  const items = new Map<string, HTMLElement>();
  let showing = "";
  const showPage = async (name: string) => {
    const pg = PAGES.find((p) => p.name === name) ?? PAGES[0];
    showing = pg.name;
    for (const [n, b] of items) b.classList.toggle("active", n === pg.name);
    title.textContent = pg.name;
    desc.textContent = pg.desc;
    const el = await pg.make(app);
    // 빨리 다른 페이지를 눌렀으면 늦게 온 것은 버린다
    if (showing !== pg.name) return;
    content.replaceChildren(cards(el));
    content.scrollTop = 0;
  };
  for (const pg of PAGES) {
    const b = h("button", { type: "button", class: "settings-item", onclick: (e: Event) => { e.preventDefault(); void showPage(pg.name); } }, icon(pg.icon), h("span", {}, pg.name));
    items.set(pg.name, b);
    nav.append(b);
  }
  const body = h("div", { class: "settings" }, nav, h("section", { class: "settings-main" }, h("header", { class: "settings-head" }, title, desc), content));
  void showPage(page);
  await modal("설정", body, [{ label: "닫기", value: true, kind: "primary" }], { cls: "settings-dialog" });
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
  const align = sel(p.alignOps ? "on" : "off", [["off", "맞추지 않음"], ["on", "맞춤 (a.x   = 1 · b.name LIKE … · SET 의 =)"]]);
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
  const fAlign = field("조건의 연산자 맞추기", align, "WHERE·ON·HAVING 의 줄마다 = · <> · >= · LIKE · IN · BETWEEN · IS NULL 을 같은 칸에 둡니다");
  const read = (): FormatPrefs => ({
    style: style.value as FormatPrefs["style"],
    keywordCase: kw.value as FormatPrefs["keywordCase"],
    identCase: ident.value as FormatPrefs["identCase"],
    commaBefore: comma.value === "before",
    indent: ind.value === "4" ? 4 : 2,
    expressionWidth: Math.max(20, Math.min(200, Number(width.value) || DEFAULT_PREFS.expressionWidth)),
    logicalBefore: logic.value === "before",
    alignOps: align.value === "on",
  });
  const set = (q: Partial<FormatPrefs>) => {
    if (q.style) style.value = q.style;
    if (q.keywordCase) kw.value = q.keywordCase;
    if (q.identCase) ident.value = q.identCase;
    if (q.commaBefore !== undefined) comma.value = q.commaBefore ? "before" : "after";
    if (q.logicalBefore !== undefined) logic.value = q.logicalBefore ? "before" : "after";
    if (q.alignOps !== undefined) align.value = q.alignOps ? "on" : "off";
    if (q.indent) ind.value = String(q.indent);
    if (q.expressionWidth) width.value = String(q.expressionWidth);
  };
  const update = () => {
    const prefs = read();
    savePrefs(prefs);
    fComma.hidden = prefs.style === "left";
    fInd.hidden = prefs.style !== "left";
    fWidth.hidden = prefs.style === "river";
    fAlign.hidden = prefs.style === "left";
    const hint = fWidth.querySelector("small");
    if (hint) hint.textContent = prefs.style === "left" ? "괄호 식이 이보다 길면 여러 줄로 나눕니다" : "목록을 이 너비까지 한 줄에 채웁니다";
    const r = formatSql(sample, prefs);
    preview.textContent = r.ok ? r.text : r.reason;
  };
  // 스타일을 고르면 그 스타일의 기본 모양으로 (대소문자는 그대로)
  style.addEventListener("change", () => { set(STYLE_PRESETS[style.value as FormatPrefs["style"]].prefs); update(); });
  for (const el of [kw, ident, comma, ind, logic, align, width]) el.addEventListener("change", update);
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
    field("키워드", kw), field("테이블·컬럼 이름", ident), fComma, field("AND / OR 위치", logic), fAlign, fInd, fWidth,
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
    h("label", { class: "check" }, (() => {
      const cb = h("input", { type: "checkbox", checked: tabsSidePref() }) as HTMLInputElement;
      cb.onchange = () => { setTabsSidePref(cb.checked); app.placeTabs(); };
      return cb;
    })(), "문서 탭을 왼쪽 목록으로 — 왼쪽의 '문서' 에 열린 탭을 세로로 보이고, 왼쪽을 접거나 다른 화면(객체·개요·프로젝트·실행 이력)을 보면 편집기 위 탭 줄로 돌아옵니다. 끄면 늘 위 탭 줄"),
    h("h3", {}, "결과 영역"),
    h("label", { class: "check" }, (() => {
      const cb = h("input", { type: "checkbox", checked: revealEnabled() }) as HTMLInputElement;
      cb.onchange = () => setRevealEnabled(cb.checked);
      return cb;
    })(), "SELECT 결과가 나오면 결과 영역 키우기 — 결과가 작게 보일 때만 화면의 절반 남짓으로 키우고, 커서는 에디터에 둡니다. 결과 영역의 ↕ 나 경계선 더블클릭으로 직접 정한 비율로 돌아갑니다"),
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
  let groups: Group[] = [];
  /** 지금 오른쪽에 연 것 — 목록을 다시 그려도 표시가 남게 */
  let current = "";
  const groupSelect = (value: string | null | undefined, allow: (g: Group) => boolean = () => true, none = "(그룹 없음)") =>
    h("select", {}, h("option", { value: "" }, none),
      ...groups.filter(allow).map((g) => h("option", { value: g.path, selected: !!value && g.path.toUpperCase() === value.toUpperCase() }, groupLabel(g.path))));
  const edit = (p?: Profile, inGroup?: string) => {
    current = p ? `p:${p.name}` : "p:";
    markActive(list, current);
    const name = h("input", { value: p?.name ?? "" });
    const group = groupSelect(p ? p.group : inGroup);
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
      field("이름", name), field("그룹", group, groups.length ? undefined : "그룹은 목록 아래 '+ 새 그룹' 으로 만듭니다"),
      field("사용자", user), field("접속 문자열", cs, "11g 는 Instant Client 가 필요합니다 (19c 권장)"), csHint,
      h("label", { class: "check" }, ro, "읽기 전용 (SELECT 만, 서버에서도 막음)"),
      h("label", { class: "check", title: "INSERT·UPDATE·DELETE·DDL·PL/SQL 블록·COMMIT 을 실행 전에 항상 확인합니다" }, prod, "운영 DB (바꾸는 문장·커밋 전에 항상 확인)"),
      h("label", { class: "check", title: "끄면 그룹의 색을 씁니다" }, useColor, "탭 색 표시 (운영 DB 는 빨강 권장)", color),
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
              call_timeout_secs: timeout.value ? Number(timeout.value) : null, group: group.value || null,
            }, p?.name);
            toast("저장했습니다", "ok");
            current = `p:${name.value.trim()}`;
            await refreshList();
          } catch (err) { toast(errOf(err).message, "error"); }
        } }, "저장"),
        p ? h("button", { class: "danger", onclick: async (e: Event) => {
          e.preventDefault();
          if (!(await confirmBox("삭제", `${p.name} 프로필을 지울까요?`, "삭제"))) return;
          await api.deleteProfile(p.name).catch((err) => toast(errOf(err).message, "error"));
          form.replaceChildren();
          current = "";
          await refreshList();
        } }, "삭제") : null));
  };
  /** 그룹 고치기 — 이름·상위 그룹·색, 하위 그룹·접속 추가, 안의 접속 한꺼번에 운영 DB 표시, 삭제 */
  const editGroup = (n?: GroupNode, parent?: string) => {
    const g = n?.group;
    current = g ? `g:${g.path}` : "g:";
    markActive(list, current);
    const name = h("input", { value: g ? groupLeaf(g.path) : "", placeholder: "예: ERP" });
    // 두 단계까지 — 하위 그룹이 있는 그룹은 다른 그룹 아래로 못 간다
    const hasChildren = !!n?.children.length;
    const up = groupSelect(g ? groupParent(g.path) : parent, (x) => !x.path.includes("/") && (!g || x.path.toUpperCase() !== g.path.toUpperCase()), "(맨 위)");
    up.disabled = hasChildren;
    const color = h("input", { type: "color", value: g?.color ?? "#3a7bd5" });
    const useColor = h("input", { type: "checkbox", checked: !!g?.color });
    const inside: Profile[] = [];
    const collect = (x: GroupNode) => { inside.push(...x.profiles); x.children.forEach(collect); };
    if (n) collect(n);
    const save = async (e: Event) => {
      e.preventDefault();
      const leaf = name.value.trim();
      if (!leaf) return toast("그룹 이름을 쓰세요", "error");
      if (leaf.includes("/")) return toast("그룹 이름에는 / 를 쓸 수 없습니다 — 상위 그룹은 아래 칸에서 고르세요", "error");
      const path = up.value ? `${up.value}/${leaf}` : leaf;
      try {
        await api.saveGroup(path, useColor.checked ? color.value : null, g?.path);
        toast("저장했습니다", "ok");
        current = `g:${path}`;
        await refreshList();
      } catch (err) { toast(errOf(err).message, "error"); }
    };
    const remove = async (e: Event) => {
      e.preventDefault();
      if (!g) return;
      const where = groupParent(g.path) ? `상위 그룹 '${groupLabel(groupParent(g.path)!)}'` : "그룹 없음";
      const what = await modal<"keep" | "all">(`'${groupLabel(g.path)}' 그룹 삭제`,
        h("pre", { class: "modal-text" }, inside.length
          ? `이 그룹${hasChildren ? "과 하위 그룹" : ""}에 접속이 ${inside.length}개 있습니다.\n\n· 그룹만 삭제: 접속은 ${where}으로 옮깁니다\n· 접속도 함께 삭제: ${inside.map((p) => p.name).join(", ")}`
          : `빈 그룹입니다${hasChildren ? " (하위 그룹도 지웁니다)" : ""}.`),
        inside.length
          ? [{ label: "취소", value: null as never }, { label: "그룹만 삭제", value: "keep", kind: "primary" }, { label: "접속도 함께 삭제", value: "all", kind: "danger" }]
          : [{ label: "취소", value: null as never }, { label: "삭제", value: "keep", kind: "danger" }]);
      if (!what) return;
      if (what === "all" && !(await confirmBox("접속도 삭제", `접속 ${inside.length}개를 지웁니다. 되돌릴 수 없습니다.`, "삭제"))) return;
      try {
        await api.deleteGroup(g.path, what === "all");
        form.replaceChildren();
        current = "";
        await refreshList();
      } catch (err) { toast(errOf(err).message, "error"); }
    };
    /** 안의 접속을 한꺼번에 — 운영 DB·읽기 전용 표시 */
    const bulk = async (label: string, change: Partial<Profile>) => {
      if (!(await confirmBox(label, `접속 ${inside.length}개: ${inside.map((p) => p.name).join(", ")}`, "바꾸기", false))) return;
      try {
        for (const p of inside) {
          const { has_env_password: _a, has_saved_password: _b, ...rest } = p;
          await api.saveProfile({ ...rest, ...change }, p.name);
        }
        toast(`${inside.length}개를 바꿨습니다`, "ok");
        await refreshList();
      } catch (err) { toast(errOf(err).message, "error"); await refreshList(); }
    };
    const small = (label: string, run: () => void, title?: string) =>
      h("button", { class: "small", title, onclick: (e: Event) => { e.preventDefault(); run(); } }, label);
    form.replaceChildren(
      h("h4", { class: "form-title" }, g ? `그룹 · ${groupLabel(g.path)}` : "새 그룹"),
      field("이름", name), field("상위 그룹", up, hasChildren ? "하위 그룹이 있어 다른 그룹 아래로 옮길 수 없습니다 (두 단계까지)" : "두 단계까지 (예: ERP / 운영)"),
      h("label", { class: "check", title: "색을 정하지 않은 접속은 그룹의 색으로 탭에 보입니다 (하위 그룹은 위 그룹 색을 이어받습니다)" }, useColor, "그룹 색", color),
      g ? h("small", {}, `접속 ${inside.length}개${hasChildren ? ` · 하위 그룹 ${n!.children.length}개` : ""}`) : "",
      g ? h("div", { class: "row" },
        small("+ 이 그룹에 새 접속", () => edit(undefined, g.path)),
        !g.path.includes("/") ? small("+ 하위 그룹", () => editGroup(undefined, g.path)) : "") : "",
      g && inside.length ? h("div", { class: "row" },
        small("모두 운영 DB 로", () => void bulk("운영 DB 로 표시", { production: true }), "바꾸는 문장·커밋 전에 항상 확인"),
        small("운영 DB 해제", () => void bulk("운영 DB 표시 해제", { production: false })),
        small("모두 읽기 전용", () => void bulk("읽기 전용으로", { read_only: true })),
        small("읽기 전용 해제", () => void bulk("읽기 전용 해제", { read_only: false }))) : "",
      h("div", { class: "row" },
        h("button", { class: "primary", onclick: save }, "저장"),
        g ? h("button", { class: "danger", onclick: remove }, "삭제") : null));
    name.focus();
  };
  const move = async (run: Promise<void>) => {
    try { await run; await refreshList(); } catch (err) { toast(errOf(err).message, "error"); }
  };
  /** 줄 끝의 ↑ ↓ — 같은 단계 안에서 자리 바꾸기 */
  const arrows = (up: () => Promise<void>, down: () => Promise<void>, first: boolean, last: boolean) =>
    h("span", { class: "cg-move" },
      h("button", { class: "icon", title: "위로", disabled: first, onclick: (e: Event) => { e.preventDefault(); e.stopPropagation(); void move(up()); } }, "↑"),
      h("button", { class: "icon", title: "아래로", disabled: last, onclick: (e: Event) => { e.preventDefault(); e.stopPropagation(); void move(down()); } }, "↓"));
  const profileRow = (p: Profile, depth: number, i: number, n: number) =>
    h("div", { class: "list-item cg-item", "data-name": `p:${p.name}`, style: `padding-left:${10 + depth * 16}px`, tabindex: "0",
      onclick: () => edit(p), onkeydown: (e: KeyboardEvent) => { if (e.key === "Enter") edit(p); } },
      h("span", { class: "dot", style: `background:${colorOf(p, groups) ?? "transparent"}` }),
      h("span", { class: "cg-name" }, p.name, h("small", {}, ` ${p.user}@${p.connect_string}`)),
      arrows(() => api.moveProfile(p.name, true), () => api.moveProfile(p.name, false), i === 0, i === n - 1));
  const groupRows = (n: GroupNode, depth: number, i: number, count: number): HTMLElement[] => {
    const path = n.group.path;
    const closed = isCollapsed(path);
    const row = h("div", { class: `list-item cg-group${closed ? " closed" : ""}`, "data-name": `g:${path}`, style: `padding-left:${4 + depth * 16}px`, tabindex: "0",
      onclick: () => editGroup(n), onkeydown: (e: KeyboardEvent) => {
        if (e.key === "Enter") editGroup(n);
        else if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); setCollapsed(path, e.key === "ArrowLeft"); void refreshList(); }
      } },
      h("button", { class: "icon cg-fold", title: closed ? "펼치기" : "접기", onclick: (e: Event) => {
        e.preventDefault(); e.stopPropagation(); setCollapsed(path, !closed); void refreshList();
      } }, closed ? "▸" : "▾"),
      h("span", { class: "dot", style: `background:${n.group.color ?? "transparent"}` }),
      h("span", { class: "cg-name" }, groupLeaf(path), h("small", {}, ` ${countIn(n)}`)),
      arrows(() => api.moveGroup(path, true), () => api.moveGroup(path, false), i === 0, i === count - 1));
    if (closed) return [row];
    return [row,
      ...n.children.flatMap((c, k) => groupRows(c, depth + 1, k, n.children.length)),
      ...n.profiles.map((p, k) => profileRow(p, depth + 1, k, n.profiles.length))];
  };
  const refreshList = async () => {
    const [ps, gs] = await Promise.all([api.listProfiles(), api.listGroups().catch(() => [] as Group[])]);
    groups = gs;
    const { roots, loose } = groupTree(groups, ps);
    list.replaceChildren(
      ...roots.flatMap((n, i) => groupRows(n, 0, i, roots.length)),
      ...loose.map((p, i) => profileRow(p, 0, i, loose.length)),
      h("button", { class: "list-item add", "data-name": "p:", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 접속"),
      h("button", { class: "list-item add", "data-name": "g:", onclick: (e: Event) => { e.preventDefault(); editGroup(); } }, "+ 새 그룹"));
    markActive(list, current);
    // 툴바 접속 선택·접속 메뉴도 새 순서로
    await app.reloadProfiles().catch(() => {});
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
    tns.dir ? h("span", { title: tns.configured && tns.configured !== tns.dir ? `설정: ${tns.configured} (프로그램 폴더 기준 상대 경로)` : tns.dir },
      `${tns.dir} · 별칭 ${tns.entries.length}개${tns.configured ? (tns.configured !== tns.dir ? ` (설정: ${tns.configured})` : "") : " (자동으로 찾음)"}`)
      : h("span", { class: "warn" }, "찾지 못했습니다 — TNS_ADMIN·ORACLE_HOME·Instant Client 폴더를 봤습니다"),
    tns.error ? h("span", { class: "warn" }, ` · 읽기 실패: ${tns.error}`) : "",
    h("button", { class: "small", onclick: pickTns }, "폴더 지정"),
    tns.configured ? h("button", { class: "small", onclick: async (e: Event) => { e.preventDefault(); await api.setTnsAdmin(null); toast("지정을 지웠습니다 — 다시 시작하면 기본 위치를 씁니다", "ok"); } }, "지정 해제") : "");
  return h("div", {},
    h("h4", {}, "접속 프로필"), h("p", { class: "hint" }, "그룹은 두 단계까지 · ▸ 로 접고 펴기 · ↑ ↓ 로 같은 단계 안에서 순서 바꾸기 · 그룹을 누르면 이름·색·삭제"),
    h("div", { class: "split" }, list, form),
    h("h4", {}, "tnsnames.ora"), h("p", { class: "hint" }, "접속 문자열 칸에 TNS 별칭을 쓰면 이 파일에서 찾습니다."), tnsBox, tnsList);
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
    markActive(list, p?.name ?? "");
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
    const budget = h("input", { type: "number", min: "1000", step: "1000", value: p?.context_budget ?? "", placeholder: "자동" }) as HTMLInputElement;
    const effort = h("select", {}, ...["", "low", "medium", "high", "xhigh", "max"].map((x) => h("option", { value: x, selected: (p?.effort ?? "") === x }, x || "(기본)")));
    // 생각(thinking) — Ollama 는 끄기·켜기·수준, OpenAI 호환은 끄기·수준(reasoning_effort)
    const thinkOpts: [string, string][] = [["", "(자동) — 로컬 모델은 SQL 생성·설명엔 끄고, 튜닝·오류 수정엔 모델 기본"], ["off", "끄기 — 생각 없이 바로 답 (빠름)"], ["on", "켜기"], ["low", "낮음 (low)"], ["medium", "중간 (medium)"], ["high", "높음 (high)"]];
    const think = h("select", {}, ...thinkOpts.map(([v, l]) => h("option", { value: v, selected: (p?.think ?? "") === v }, l))) as HTMLSelectElement;
    const fallbacks = h("input", { type: "checkbox", checked: p?.fallbacks ?? true });
    const maxTokens = h("input", { type: "number", value: p?.max_tokens ?? 8192 });
    // 사내 PC 는 시스템 프록시가 잡혀 있어 사내 LLM 서버로 가는 요청까지 프록시로 나가 막히곤 한다 (브라우저는 PAC 로 직접 연결)
    const proxy = h("select", {},
      h("option", { value: "", selected: !p?.proxy || p.proxy === "auto" }, "자동 — 사내 주소(사설 IP·점 없는 이름·.local/.corp 등)는 직접, 그 밖은 시스템 프록시"),
      h("option", { value: "none", selected: p?.proxy === "none" }, "사용 안 함 — 항상 직접 연결"),
      h("option", { value: "system", selected: p?.proxy === "system" }, "시스템 프록시 사용"));
    const result = h("pre", { class: "modal-text" });
    const profileNote = h("span", { class: "hint" }, p?.profile && p.profile.model === p.model ? profileText(p.profile) : "아직 재지 않음");
    const collect = (): Provider => ({
      name: name.value.trim(), kind: kind.value as Provider["kind"], base_url: base.value.trim() || null,
      model: model.value.trim(), api_key_env: keyEnv.value.trim() || null, num_ctx: numCtx.value ? Number(numCtx.value) : null,
      effort: effort.value || null, fallbacks: fallbacks.checked, max_tokens: Number(maxTokens.value) || 8192,
      proxy: proxy.value || null, think: kind.value === "anthropic" ? null : think.value || null,
      context_budget: Number(budget.value) > 0 ? Math.floor(Number(budget.value)) : null,
      // 화면에 칸이 없는 값은 그대로 둔다 (설정 파일에서 직접 정한 것)
      temperature: p?.temperature ?? null, keep_alive: p?.keep_alive ?? null,
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
      ctxField,
      field("문맥 예산 (토큰)", budget, "스키마·문법 안내·대화 기록을 합쳐 이만큼까지 넣고, 넘치면 컬럼 많은 테이블은 관련 컬럼만 → 내 쿼리 예시 → 문법 안내 → 테이블 순서로 줄입니다. 비우면 자동: 로컬은 num_ctx 의 60% 와 8000 중 작은 값 (작은 모델은 문맥이 길면 중간을 놓칩니다)"),
      thinkField, effortField,
      field("모델 측정", profileNote, "'모델 측정' 버튼으로 잽니다. 모델을 바꾸면 다시 재야 씁니다"),
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
        h("button", { title: "짧은 질문 3개로 속도·형식(JSON) 지정·생각 모드·문맥 크기를 재서 저장합니다. 하네스가 이 값으로 단계를 고릅니다 (느린 모델은 중간 단계를 건너뜀)", onclick: async (e: Event) => {
          e.preventDefault();
          try {
            const np = await persist();
            result.textContent = "모델 측정 중… (질문 3개 — 모델 로딩까지 1분쯤 걸릴 수 있습니다)";
            result.classList.remove("warn");
            const prof = await api.aiProbe(np.name);
            profileNote.textContent = profileText(prof);
            result.textContent = `측정했습니다 — ${profileText(prof)}`;
            await refreshList();
          } catch (err) { result.textContent = errOf(err).message; }
        } }, "모델 측정"),
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
      ...ps.map((p) => h("button", { class: "list-item", "data-name": p.name, onclick: (e: Event) => { e.preventDefault(); edit(p); } },
        `${p.name}`, h("small", {}, ` ${p.model}${p.remote ? " · 외부" : " · 로컬"}`))),
      h("button", { class: "list-item add", "data-name": "", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 공급자"));
  };
  await refreshList();
  return h("div", {}, h("h4", {}, "AI 공급자"), h("div", { class: "split" }, list, form),
    h("h4", {}, "ACP 에이전트 (opencode 등)"), ...(await acpSection(app)),
    h("h4", {}, "하네스"),
    h("p", { class: "hint" }, "테이블 고르기 · 문법 카드 · 내 쿼리 예시 · 검증처럼 질문 하나가 거치는 단계와 그 설정은 설정 → 하네스 에서 카드로 봅니다."));
}

/** 공급자 하나에서 하네스가 고른 단계 (모델 측정값으로) */
interface HarnessPlan {
  local: boolean; routing: boolean; think: boolean; repairs: number; tables: number; tok_per_sec?: number | null;
  slow_below: number; budget: number; user_think?: string | null; user_temperature?: number | null;
}

const TASK_NAME: Record<Task, string> = { generate: "SQL 생성", explain: "설명", optimize: "튜닝", fix: "오류 수정", chat: "질문", trace: "출처 설명" };
const ALL_TASKS: Task[] = ["generate", "explain", "optimize", "fix", "chat", "trace"];

/** 하네스 카드 하나 — on: 늘 · auto: 상황 따라 · measure: 모델 측정값 · off: 이 공급자에선 건너뜀 */
interface HarnessCard { phase: string; name: string; what: string; tasks: Task[]; state: string; tone: "on" | "auto" | "measure" | "off"; meta: string; control?: Node[] }

/** 하네스 — AI 질문 하나가 거치는 단계를 카드로. 고른 공급자에서 실제로 무엇이 도는지와 그 설정 */
async function harnessPage(app: App) {
  const [providers, agents, ho, gi] = await Promise.all([
    api.listProviders().catch(() => [] as Provider[]),
    api.acpAgents().catch(() => [] as AcpAgent[]),
    api.harnessOpts().catch(() => null),
    invoke<{ path: string; exists: boolean; builtin: number; custom: number }>("ai_guides_info").catch(() => null),
  ]);
  let guideMode = await invoke<string>("ai_guide_select").catch(() => "auto");
  const saved = () => toast("저장했습니다", "ok", 1200);
  const fail = (e: unknown) => toast(errOf(e).message, "error");

  // 단계 카드 안의 설정 — 바꾸면 바로 저장하고 카드를 다시 그린다
  const pickSel = h("select", {}, h("option", { value: "auto" }, "자동"), h("option", { value: "off" }, "끔")) as HTMLSelectElement;
  pickSel.value = ho?.table_pick ?? "auto";
  pickSel.addEventListener("change", () => { api.setHarnessOpts({ tablePick: pickSel.value }).then(() => { if (ho) ho.table_pick = pickSel.value as "auto" | "off"; saved(); void draw(); }).catch(fail); });
  const guideSel = h("select", {}, h("option", { value: "auto" }, "자동 (낱말로 약할 때만)"), h("option", { value: "always" }, "항상"), h("option", { value: "off" }, "끔 (낱말로만)")) as HTMLSelectElement;
  guideSel.value = guideMode;
  guideSel.addEventListener("change", () => { invoke("set_ai_guide_select", { mode: guideSel.value }).then(() => { guideMode = guideSel.value; saved(); void draw(); }).catch(fail); });
  const exOn = h("input", { type: "checkbox", checked: ho?.examples ?? true }) as HTMLInputElement;
  exOn.addEventListener("change", () => { api.setHarnessOpts({ examples: exOn.checked }).then(() => { if (ho) ho.examples = exOn.checked; saved(); void draw(); }).catch(fail); });
  let exCount = ho?.examples_count ?? 0;
  const clearEx = h("button", { title: ho?.examples_path ?? "", onclick: async (e: Event) => {
    e.preventDefault();
    if (!(await confirmBox("내 쿼리 예시 지우기", "쌓아 둔 예시를 모두 지울까요?", "지우기"))) return;
    try { await api.clearExamples(); exCount = 0; toast("지웠습니다", "ok"); void draw(); } catch (err) { fail(err); }
  } }, "예시 지우기");
  const guideBtns = [
    h("button", { title: "AI 에 보내는 문법 안내 카드를 모두 보고, 기본 카드를 회사 규칙으로 옮겨 고친다", onclick: (e: Event) => { e.preventDefault(); void showGuides(app); } }, "카드 보기"),
    h("button", { title: gi?.path ?? "", onclick: async (e: Event) => {
      e.preventDefault();
      try {
        await app.openPath(await invoke<string>("ai_guides_edit"));
        toast("편집하고 저장하면 다음 질문부터 씁니다", "info", 5000);
      } catch (err) { fail(err); }
    } }, gi?.exists ? "회사 규칙 편집" : "회사 규칙 추가"),
  ];

  const last = localStorage.getItem("ai.provider");
  const provSel = h("select", { class: "hn-provider" }, ...providers.map((p) => h("option", { value: p.name, selected: p.name === last },
    `${p.name} · ${p.model} (${p.remote ? "외부" : "로컬"}${p.profile ? ` · ${p.profile.tok_per_sec.toFixed(0)} tok/s` : " · 측정 전"})`))) as HTMLSelectElement;
  let filter: Task | "" = "";
  const chips = h("div", { class: "hn-tasks", role: "group", "aria-label": "작업으로 거르기" });
  const drawChips = () => chips.replaceChildren(h("span", { class: "hint" }, "작업"),
    ...(["", ...ALL_TASKS] as (Task | "")[]).map((t) => h("button", { class: "hn-chip", "aria-pressed": String(filter === t),
      onclick: (e: Event) => { e.preventDefault(); filter = t; drawChips(); void draw(); } }, t ? TASK_NAME[t] : "전체")));
  drawChips();
  const grid = h("div", { class: "hn-grid" });
  const note = h("p", { class: "hint" });

  const draw = async () => {
    const p = providers.find((x) => x.name === provSel.value);
    if (!p) {
      grid.replaceChildren(h("p", { class: "hint" }, "AI 공급자를 먼저 추가하세요 (설정 → AI)."));
      return;
    }
    let pl: HarnessPlan;
    try { pl = await invoke<HarnessPlan>("ai_harness_plan", { provider: p.name }); } catch (e) { fail(e); return; }
    const slow = pl.local && !pl.routing;
    note.textContent = pl.tok_per_sec == null
      ? "아직 모델을 측정하지 않아 4B 급 기본값으로 단계를 고릅니다 — 설정 → AI 의 공급자에서 '모델 측정'을 누르세요."
      : `측정: ${pl.tok_per_sec.toFixed(1)} tok/s${slow ? ` — ${pl.slow_below} tok/s 아래라 느린 모델로 보고 중간 단계를 줄입니다` : ""}.`;
    const card = (c: HarnessCard) => {
      const off = c.tone === "off" || (filter !== "" && !c.tasks.includes(filter));
      return h("section", { class: `hn-card${off ? " off" : ""}`, "data-tone": c.tone },
        h("div", { class: "hn-card-head" }, h("span", {}, c.phase), h("span", { class: "hn-state" }, c.state)),
        h("h5", {}, c.name),
        h("p", {}, c.what),
        h("div", { class: "hn-meta" }, `${c.tasks.length === ALL_TASKS.length ? "모든 작업" : c.tasks.map((t) => TASK_NAME[t]).join(" · ")}${c.meta ? ` · ${c.meta}` : ""}`),
        ...(c.control?.length ? [h("div", { class: "hn-control" }, ...c.control)] : []));
    };
    const pickOff = ho?.table_pick === "off";
    const exState = !pl.local ? "외부라 안 씀" : !(ho?.examples ?? true) ? "꺼짐" : "켜짐";
    const thinkUser = pl.user_think != null || pl.user_temperature != null;
    grid.replaceChildren(...[
      { phase: "1 · 문맥", name: "테이블 고르기", tasks: ["generate"],
        what: "SQL 생성인데 테이블을 모르면 질문 낱말로 사전에서 후보를 찾고, 모델이 고릅니다.",
        state: pickOff ? "꺼짐" : slow ? "낱말로만" : "자동", tone: pickOff ? "off" : "auto", meta: `최대 ${pl.tables}개`,
        control: [h("label", { class: "hn-inline" }, "설정 ", pickSel)] },
      { phase: "2 · 문맥", name: "스키마 읽기", tasks: ALL_TASKS,
        what: "SQL 이 쓰는 테이블과 고른 테이블의 컬럼·주석·인덱스를 읽어 넣습니다.", state: "늘", tone: "on", meta: "탭이 접속해 있을 때" },
      { phase: "3 · 문맥", name: "실행계획", tasks: ["optimize"],
        what: "튜닝이면 지금 접속으로 EXPLAIN PLAN 을 만들어 넣습니다. 실행은 하지 않습니다.", state: "늘", tone: "on", meta: "ACP 에도" },
      { phase: "4 · 문맥", name: "문법 카드", tasks: ALL_TASKS.filter((t) => t !== "trace"),
        what: "질문·SQL·오류 낱말로 Oracle 문법 카드를 고르고, 설정에 따라 모델에게도 고르게 합니다.",
        state: guideMode === "off" ? "낱말로만" : guideMode === "always" ? "항상 AI" : slow ? "낱말로만" : "자동", tone: "auto",
        meta: `카드 ${gi?.builtin ?? "?"}장${gi?.custom ? ` + 회사 규칙 ${gi.custom}장` : ""}`,
        control: [h("label", { class: "hn-inline" }, "AI 고르기 ", guideSel), ...guideBtns] },
      { phase: "5 · 문맥", name: "내 쿼리 예시", tasks: ["generate", "chat"],
        what: "서버 파싱을 통과해 에디터에 넣은 AI 의 SQL 가운데 비슷한 질문의 것을 2개까지 넣습니다.",
        state: exState, tone: exState === "켜짐" ? "auto" : "off", meta: `로컬만 · 쌓인 예시 ${exCount}개`,
        control: [h("label", { class: "check" }, exOn, "쓰기"), clearEx] },
      { phase: "6 · 보내기", name: "예산 맞추기", tasks: ALL_TASKS,
        what: "모델 문맥에 맞게 덜 중요한 컬럼·카드·지난 대화부터 줄입니다. 줄인 것은 답의 '예산' 칩에 보입니다.",
        state: "늘", tone: "on", meta: `${pl.budget.toLocaleString()} 토큰` },
      { phase: "7 · 보내기", name: "생각·온도", tasks: ["generate", "explain", "chat", "trace"],
        what: "작은 로컬 모델은 SQL 생성·설명에서 생각을 끄고 온도를 낮춥니다. 같은 말을 되풀이하면 생각을 끄고 다시 묻습니다.",
        state: !pl.local ? "외부라 안 씀" : thinkUser ? "공급자 설정" : pl.think ? "측정값" : "생각 조절 없음", tone: !pl.local ? "off" : "measure",
        meta: thinkUser ? `think ${pl.user_think ?? "기본"} · temperature ${pl.user_temperature ?? "기본"}` : "로컬만" },
      { phase: "8 · 받은 뒤", name: "검증·수리", tasks: ["generate", "optimize", "fix", "chat"],
        what: "답의 SQL 을 버전 문법으로, 다음 서버에서 파싱만 해 봅니다. 걸리면 오류와 전체 컬럼을 주고 고쳐 달라고 다시 묻습니다.",
        state: `최대 ${pl.repairs}번`, tone: "measure", meta: slow ? "느린 모델이라 1번" : "ACP 는 1번" },
    ].map((c) => card(c as HarnessCard)));
  };
  provSel.addEventListener("change", () => void draw());
  await draw();

  const acp = h("div", { class: "hn-acp", role: "list" }, ...[
    ["앱", "연결", "띄우기(opencode 는 --pure) · 초기화 · 앱을 켤 때 미리"],
    ["앱", "세션", "저장한 모델 · 처음 모드 · MCP 접속"],
    ["앱", "보낸 문맥", "규칙 · SQL · 오류 · 튜닝이면 실행계획"],
    ["에이전트", "에이전트 작업", "생각 · 도구 호출(사전 · explain_plan) · 권한 묻기"],
    ["앱", "검증·수리", "답 SQL 서버 파싱 · 걸리면 한 번 다시 물음"],
  ].map(([who, name, what]) => h("div", { class: `hn-acp-step${who === "앱" ? "" : " agent"}`, role: "listitem" }, h("span", {}, who), h("b", {}, name), h("span", {}, what))));

  return h("div", {},
    h("h4", {}, "질문 하나가 거치는 단계"),
    h("p", { class: "hint" }, "고른 공급자에서 실제로 도는 단계입니다. 흐린 카드는 이 공급자나 고른 작업에서는 건너뜁니다. AI 답 위의 단계 칩이 같은 순서로 무엇을 했는지 보여 줍니다."),
    h("div", { class: "hn-bar" }, h("label", { class: "hn-inline" }, "공급자 ", provSel), chips),
    note, grid,
    h("h4", {}, "ACP 에이전트를 고르면"),
    h("p", { class: "hint" }, agents.length
      ? `${agents.map((a) => a.name).join(", ")} — 문맥은 에이전트가 MCP 로 직접 찾고, 앱은 앞뒤만 맡습니다 (테이블 고르기·문법 카드·예산 맞추기는 쓰지 않음).`
      : "ACP 에이전트(opencode 등)를 추가하면 문맥은 에이전트가 MCP 로 직접 찾고, 앱은 앞뒤만 맡습니다."),
    acp);
}

/** ACP 에이전트 — 코딩 에이전트를 자식 프로세스로 띄워 AI 패널에서 쓴다 (Agent Client Protocol) */
async function acpSection(app: App): Promise<HTMLElement[]> {
  const list = h("div", { class: "list" });
  const form = h("div", { class: "form" });
  const edit = (a?: AcpAgent) => {
    markActive(list, a?.name ?? "");
    const name = h("input", { value: a?.name ?? "", placeholder: "예: opencode" });
    const command = h("input", { value: a?.command ?? "", placeholder: "예: opencode (PATH 에서 찾음) 또는 전체 경로" });
    const args = h("input", { value: (a?.args ?? []).join(" "), placeholder: "예: acp" });
    const cwd = h("input", { value: a?.cwd ?? "", placeholder: "비우면 데이터 폴더의 acp-work" });
    const mode = h("input", { value: a?.mode ?? "", placeholder: "예: plan" });
    const model = h("input", { value: a?.model ?? "", placeholder: "비우면 에이전트 기본값" });
    const mcp = h("input", { type: "checkbox", checked: a?.mcp ?? true }) as HTMLInputElement;
    const pure = h("input", { type: "checkbox", checked: a?.pure ?? true }) as HTMLInputElement;
    const result = h("pre", { class: "modal-text" });
    const collect = (): AcpAgent => ({
      name: name.value.trim(), command: command.value.trim(), args: args.value.trim().split(/\s+/).filter(Boolean),
      cwd: cwd.value.trim() || null, mcp: mcp.checked, pure: pure.checked, mode: mode.value.trim() || null, model: model.value.trim() || null,
    });
    form.replaceChildren(
      h("p", { class: "hint" }, "ACP(Agent Client Protocol)를 지원하는 코딩 에이전트를 AI 패널의 공급자 목록에서 고를 수 있게 합니다. 로그인은 에이전트 쪽 설정을 씁니다 (opencode: 터미널에서 opencode auth login)."),
      field("이름", name), field("실행 파일", command), field("인자", args, "빈칸으로 나눕니다 (예: acp)"),
      h("label", { class: "check", title: "opencode 를 --pure 로 띄워 외부 플러그인을 읽지 않습니다 — 느린 PC 에서 시작이 크게 빨라집니다. omo(oh-my-opencode) 같은 플러그인의 에이전트를 쓰려면 끄세요" },
        pure, "플러그인 없이 빠르게 시작 (opencode --pure) — omo 같은 플러그인 에이전트를 쓰려면 끄기"),
      field("작업 폴더", cwd, "에이전트가 파일을 보는 곳"),
      field("처음 모드", mode, "새 대화마다 이 모드로 바꿉니다. opencode 는 primary 에이전트를 모드로 내놓습니다 — build · plan 과 설정·플러그인(omo 등)의 에이전트 이름. AI 패널의 모드 칸에 보이는 이름을 적습니다. plan 은 파일 편집 도구를 막고, build 와 대부분의 플러그인 에이전트는 묻지 않고 파일을 고칠 수 있습니다"),
      field("모델", model, "새 대화마다 이 모델로 바꿉니다. AI 패널에서 모델을 고르면 여기에 저장됩니다 (opencode 예: opencode/big-pickle)"),
      h("label", { class: "check", title: "새 대화를 열 때 sqlstudio-mcp 를 MCP 서버로 넘깁니다. 설정 → MCP 에서 허용한 접속의 사전(테이블·컬럼·소스)만 조회하고, SQL 은 실행하지 않습니다" },
        mcp, "DB 사전 조회 도구(sqlstudio-mcp)를 에이전트에 넘기기"),
      h("div", { class: "row" },
        h("button", { class: "primary", onclick: async (e: Event) => {
          e.preventDefault();
          try { await api.acpSaveAgent(collect(), a?.name); toast("저장했습니다", "ok"); await refresh(); await app.ai.load(); } catch (err) { toast(errOf(err).message, "error"); }
        } }, "저장"),
        h("button", { title: "저장하지 않은 설정으로 띄워 보고 바로 끝냅니다", onclick: async (e: Event) => {
          e.preventDefault();
          result.textContent = "띄워 보는 중…";
          try {
            const r = await api.acpTestAgent(collect());
            result.textContent = `연결됨 — ${r.agent?.name ?? "?"} ${r.agent?.version ?? ""}\n${r.path}` +
              (r.auth.length ? `\n로그인이 필요하면: ${r.auth.join(" / ")}` : "");
          } catch (err) { result.textContent = errOf(err).message; }
        } }, "연결 테스트"),
        a ? h("button", { class: "danger", onclick: async (e: Event) => {
          e.preventDefault();
          if (!(await confirmBox("ACP 에이전트 삭제", `'${a.name}' 을(를) 지울까요?`, "삭제"))) return;
          try { await api.acpDeleteAgent(a.name); form.replaceChildren(); toast("지웠습니다", "ok"); await refresh(); await app.ai.load(); } catch (err) { toast(errOf(err).message, "error"); }
        } }, "삭제") : null),
      result);
  };
  const refresh = async () => {
    const agents = await api.acpAgents().catch(() => [] as AcpAgent[]);
    list.replaceChildren(
      ...agents.map((a) => h("button", { class: "list-item", "data-name": a.name, onclick: (e: Event) => { e.preventDefault(); edit(a); } },
        a.name, h("small", {}, ` ${a.command} ${a.args.join(" ")}${a.available ? "" : " · 찾을 수 없음"}`))),
      h("button", { class: "list-item add", "data-name": "", onclick: (e: Event) => { e.preventDefault(); edit(); } }, "+ 새 에이전트"));
    if (!agents.length) {
      form.replaceChildren(h("p", { class: "hint" }, "opencode 를 설치하면(npm i -g opencode-ai) 여기에 저절로 나타납니다. 다른 ACP 에이전트는 '+ 새 에이전트' 로 추가하세요."));
    }
  };
  await refresh();
  return [h("div", { class: "split" }, list, form)];
}

/** 모델 측정 결과 한 줄 — 하네스가 무엇을 바꾸는지까지 */
function profileText(m: ModelProfile) {
  const slow = m.tok_per_sec > 0 && m.tok_per_sec < 8;
  return [
    m.tok_per_sec > 0 ? `${m.tok_per_sec} tok/s${slow ? " (느림 — 중간 단계를 건너뛰고 고치기는 한 번)" : ""}` : "속도 모름",
    `JSON 형식 ${m.json ? "따름" : "못 따름 (형식 없이 보냄)"}`,
    `생각 모드 ${m.think ? "있음" : "없음"}`,
    m.context ? `모델 문맥 ${m.context.toLocaleString()}` : "",
  ].filter(Boolean).join(" · ");
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
