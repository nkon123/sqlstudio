// 데이터 가져오기 창 — 엑셀·CSV 를 골라 미리 보고, 테이블 컬럼에 맞춰 넣는다.
//
// 1) 파일: 시트·첫 줄 머리글·앞 줄 건너뛰기·구분자·인코딩 (바꾸면 바로 다시 읽음)
// 2) 대상 컬럼마다 파일의 어느 열인지 (이름이 같으면 자동), 날짜 컬럼은 형식 (값을 보고 짐작)
// 3) 가져오기: 500행씩 묶어 넣는다. 커밋하지 않는다 — 결과를 보고 툴바의 커밋/롤백.

import { Channel, invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api, errOf, type TableDesc } from "./api";
import { confirmBox, h, toast } from "./ui";

interface Table { sheets: string[]; sheet?: string | null; encoding?: string | null; delimiter?: string | null; headers: string[]; rows: (string | null)[][] }
interface Summary { total: number; inserted: number; failed: number; stopped: boolean; errors: [number, string][]; txn_pending: boolean; sql: string }
type Ev = { type: "progress"; done: number; total: number; inserted: number; failed: number };

export interface ImportHost {
  id: number;
  owner: string;
  table: string;
  /** 끝난 뒤 (탭의 커밋 표시 갱신) */
  done: (s: Summary) => void;
}

const norm = (s: string) => s.toUpperCase().replace(/[^A-Z0-9가-힣]/g, "");

function kindOf(dataType: string) {
  const t = dataType.toUpperCase();
  if (t.startsWith("TIMESTAMP")) return "timestamp";
  if (t === "DATE") return "date";
  if (/^(NUMBER|FLOAT|INTEGER|BINARY_|DECIMAL)/.test(t)) return "number";
  return "text";
}

/** 표본 값으로 Oracle 날짜 형식 짐작 */
export function guessFormat(samples: string[], kind: string) {
  const v = samples.map((s) => s.trim()).filter(Boolean);
  const all = (re: RegExp) => v.length > 0 && v.every((s) => re.test(s));
  const frac = kind === "timestamp" && all(/\.\d+$/) ? ".FF" : "";
  if (all(/^\d{4}-\d{2}-\d{2} \d{1,2}:\d{2}:\d{2}(\.\d+)?$/)) return `YYYY-MM-DD HH24:MI:SS${frac}`;
  if (all(/^\d{4}-\d{2}-\d{2} \d{1,2}:\d{2}$/)) return "YYYY-MM-DD HH24:MI";
  if (all(/^\d{4}-\d{2}-\d{2}$/)) return "YYYY-MM-DD";
  if (all(/^\d{4}\/\d{2}\/\d{2} \d{1,2}:\d{2}:\d{2}$/)) return "YYYY/MM/DD HH24:MI:SS";
  if (all(/^\d{4}\/\d{1,2}\/\d{1,2}$/)) return "YYYY/MM/DD";
  if (all(/^\d{4}\.\d{1,2}\.\d{1,2}$/)) return "YYYY.MM.DD";
  if (all(/^\d{14}$/)) return "YYYYMMDDHH24MISS";
  if (all(/^\d{8}$/)) return "YYYYMMDD";
  return "YYYY-MM-DD HH24:MI:SS";
}

export async function openImport(host: ImportHost) {
  let desc: TableDesc;
  try {
    desc = await api.describe(host.id, `"${host.owner}"."${host.table}"`);
  } catch (e) {
    return toast(errOf(e).message, "error");
  }
  let path = "";
  let table: Table | null = null;
  let total = 0;
  let running = false;

  const pathEl = h("span", { class: "imp-path" }, "파일을 고르세요 (.xlsx · .xls · .csv · .txt)");
  const sheetSel = h("select", { hidden: true });
  const header = h("input", { type: "checkbox", checked: true });
  const skip = h("input", { type: "number", min: "0", value: "0", style: "width:52px", title: "앞에서 건너뛸 줄 (제목 줄 등)" });
  const delim = h("select", { title: "구분자 (CSV)" }, ...[["", "구분자 자동"], [",", "쉼표 ,"], ["\\t", "탭"], [";", "세미콜론 ;"], ["|", "세로줄 |"]].map(([v, l]) => h("option", { value: v }, l)));
  const enc = h("select", { title: "인코딩 (CSV)" }, ...[["", "인코딩 자동"], ["utf-8", "UTF-8"], ["cp949", "CP949(EUC-KR)"]].map(([v, l]) => h("option", { value: v }, l)));
  const csvOpts = h("span", { class: "imp-csv", hidden: true }, delim, enc);
  const info = h("span", { class: "imp-info" });
  const preview = h("div", { class: "imp-preview" });
  const mapBody = h("tbody", {});
  const skipErr = h("input", { type: "checkbox" });
  const bar = h("div", { class: "imp-bar-fill" });
  const status = h("div", { class: "imp-status" });
  const errBox = h("div", { class: "imp-errors" });
  const goBtn = h("button", { class: "primary", disabled: true }, "가져오기");
  const stopBtn = h("button", { class: "danger", hidden: true }, "멈춤");
  const closeBtn = h("button", {}, "닫기");

  const opts = () => ({ sheet: sheetSel.value || null, header: header.checked, skip: Number(skip.value) || 0, delimiter: delim.value || null, encoding: enc.value || null });
  // 매핑 행: 대상 컬럼 → 원본 열 선택, 형식
  const maps = desc.columns.map((c) => ({
    col: c, kind: kindOf(c.data_type),
    src: h("select", { class: "imp-src" }), fmt: h("input", { class: "imp-fmt", placeholder: "형식", style: "width:170px" }),
  }));

  const renderPreview = () => {
    if (!table) return preview.replaceChildren();
    const rows = table.rows.slice(0, 20);
    preview.replaceChildren(h("table", { class: "imp-grid" },
      h("thead", {}, h("tr", {}, h("th", {}, "#"), ...table.headers.map((x, i) => h("th", { title: `${i + 1}번 열` }, x)))),
      h("tbody", {}, ...rows.map((r, i) => h("tr", {}, h("td", { class: "num" }, String(i + 1)), ...r.map((v) => h("td", { class: v == null ? "null" : "" }, v ?? "")))))));
  };

  const renderMap = (auto: boolean) => {
    const hs = table?.headers ?? [];
    const byName = new Map(hs.map((x, i) => [norm(x), i]));
    for (const [i, m] of maps.entries()) {
      const prev = m.src.value;
      m.src.replaceChildren(h("option", { value: "" }, "(넣지 않음)"), ...hs.map((x, j) => h("option", { value: String(j) }, `${j + 1}. ${x}`)));
      // 이름이 같거나, 컬럼 주석(한글 설명)이 머리글과 같으면 자동으로
      let pick = auto ? byName.get(norm(m.col.name)) ?? (m.col.comment ? byName.get(norm(m.col.comment)) : undefined) : prev === "" ? undefined : Number(prev);
      // 머리글이 없고 열 수가 같으면 순서대로
      if (auto && pick === undefined && !header.checked && hs.length === desc.columns.length) pick = i;
      m.src.value = pick !== undefined && pick < hs.length ? String(pick) : "";
      updateFmt(m);
    }
    updateGo();
  };
  const updateFmt = (m: (typeof maps)[number]) => {
    const dated = m.kind === "date" || m.kind === "timestamp";
    m.fmt.hidden = !dated;
    if (dated && m.src.value !== "" && table && !m.fmt.dataset.touched) {
      const j = Number(m.src.value);
      m.fmt.value = guessFormat(table.rows.slice(0, 50).map((r) => r[j] ?? ""), m.kind);
    }
  };
  for (const m of maps) {
    m.src.onchange = () => { updateFmt(m); updateGo(); };
    m.fmt.oninput = () => { m.fmt.dataset.touched = "1"; };
    mapBody.append(h("tr", {}, h("td", {}, desc.primary_key.includes(m.col.name) ? h("span", { class: "cp-pk" }, "PK") : "", m.col.name),
      h("td", { class: "dim" }, m.col.data_type + (m.col.nullable ? "" : " · NOT NULL")), h("td", {}, m.src), h("td", {}, m.fmt)));
  }
  const chosen = () => maps.filter((m) => m.src.value !== "");
  const updateGo = () => {
    const n = chosen().length;
    goBtn.disabled = running || !table || !n || !total;
    goBtn.textContent = table ? `가져오기 (${total.toLocaleString()}행 × ${n}컬럼)` : "가져오기";
  };

  const read = async (auto: boolean) => {
    if (!path) return;
    info.textContent = "읽는 중…";
    try {
      const r = await invoke<{ table: Table; total: number }>("import_read", { path, opts: opts(), limit: 200 });
      table = r.table; total = r.total;
      sheetSel.hidden = !table.sheets.length;
      if (table.sheets.length && sheetSel.options.length !== table.sheets.length) {
        sheetSel.replaceChildren(...table.sheets.map((s) => h("option", { value: s, selected: s === table!.sheet }, s)));
      }
      csvOpts.hidden = !!table.sheets.length;
      info.textContent = `${total.toLocaleString()}행 · ${table.headers.length}열${table.encoding ? ` · ${table.encoding}` : ""}${table.delimiter ? ` · 구분자 ${table.delimiter === "\\t" ? "탭" : table.delimiter}` : ""}`;
      renderPreview();
      renderMap(auto);
    } catch (e) {
      table = null; total = 0;
      info.textContent = "";
      preview.replaceChildren(h("div", { class: "err" }, errOf(e).message));
      updateGo();
    }
  };
  for (const el of [sheetSel, header, skip, delim, enc]) el.addEventListener("change", () => read(el === header || el === sheetSel));

  const pick = h("button", { onclick: async () => {
    const p = await openDialog({ multiple: false, filters: [{ name: "엑셀·CSV", extensions: ["xlsx", "xlsm", "xls", "xlsb", "ods", "csv", "txt", "tsv"] }] });
    if (!p || Array.isArray(p)) return;
    path = p; pathEl.textContent = p; pathEl.title = p;
    sheetSel.replaceChildren();
    await read(true);
  } }, "파일 고르기");

  const run = async (confirmed: boolean): Promise<void> => {
    const columns = chosen().map((m) => ({ column: m.col.name, src: Number(m.src.value), kind: m.kind, format: m.fmt.hidden ? null : m.fmt.value.trim() || null }));
    const ch = new Channel<Ev>();
    ch.onmessage = (ev) => {
      bar.style.width = `${Math.round((ev.done / Math.max(1, ev.total)) * 100)}%`;
      status.textContent = `${ev.done.toLocaleString()} / ${ev.total.toLocaleString()}행 · 넣음 ${ev.inserted.toLocaleString()}${ev.failed ? ` · 실패 ${ev.failed.toLocaleString()}` : ""}`;
    };
    try {
      const s = await invoke<Summary>("import_run", {
        args: { id: host.id, path, opts: opts(), owner: desc.owner, table: desc.name, columns, skip_errors: skipErr.checked, confirmed },
        onEvent: ch,
      });
      bar.style.width = "100%";
      status.textContent = `${s.stopped ? "멈춤 — " : "끝 — "}${s.inserted.toLocaleString()}행 넣음${s.failed ? `, ${s.failed.toLocaleString()}행 실패` : ""} (전체 ${s.total.toLocaleString()}). ` +
        (s.inserted ? "아직 커밋하지 않았습니다 — 확인 후 툴바의 커밋 또는 롤백." : "");
      status.className = `imp-status ${s.failed ? "warn" : "ok"}`;
      errBox.replaceChildren(...s.errors.map(([n, m]) => h("div", {}, h("b", {}, `${n}행 `), m)));
      host.done(s);
    } catch (e) {
      const err = errOf(e);
      if (err.kind === "confirm_required" && !confirmed) {
        if (await confirmBox("운영 DB 에 넣기", err.message, "가져오기")) return run(true);
        status.textContent = "취소했습니다";
        return;
      }
      status.textContent = err.message;
      status.className = "imp-status warn";
    }
  };

  goBtn.onclick = async () => {
    if (!table) return;
    const missing = maps.filter((m) => !m.col.nullable && m.src.value === "" && !m.col.default);
    if (missing.length && !(await confirmBox("필수 컬럼", `NOT NULL 인데 넣을 열을 고르지 않은 컬럼이 있습니다:\n${missing.map((m) => `· ${m.col.name}`).join("\n")}\n\n그대로 하면 모든 행이 실패할 수 있습니다.`, "그대로 가져오기"))) return;
    running = true; updateGo();
    stopBtn.hidden = false; closeBtn.disabled = true;
    errBox.replaceChildren();
    status.className = "imp-status";
    status.textContent = "시작…";
    bar.style.width = "0";
    try { await run(false); } finally {
      running = false; updateGo();
      stopBtn.hidden = true; closeBtn.disabled = false;
    }
  };
  stopBtn.onclick = () => invoke("cancel", { id: host.id }).catch(() => {});

  const dlg = h("dialog", { class: "modal wide imp" },
    h("h3", {}, `데이터 가져오기 → ${desc.owner}.${desc.name}`),
    h("div", { class: "imp-row" }, pick, pathEl),
    h("div", { class: "imp-row" }, sheetSel, h("label", { class: "check" }, header, "첫 줄이 컬럼 이름"),
      h("label", { class: "check" }, "앞", skip, "줄 건너뛰기"), csvOpts, h("span", { class: "spacer" }), info),
    preview,
    h("h4", {}, "컬럼 맞추기 — 컬럼 이름이나 주석이 머리글과 같으면 자동으로 맞춥니다. 날짜는 Oracle 형식 (예: YYYY-MM-DD HH24:MI:SS)"),
    h("div", { class: "imp-map" }, h("table", { class: "imp-grid" },
      h("thead", {}, h("tr", {}, ...["대상 컬럼", "형식", "파일의 열", "날짜 형식"].map((x) => h("th", {}, x)))), mapBody)),
    h("div", { class: "imp-row" }, h("label", { class: "check", title: "끄면 첫 오류에서 멈춥니다 (이미 넣은 행은 커밋 전이라 롤백할 수 있음)" }, skipErr, "오류 난 행은 건너뛰고 계속"),
      h("span", { class: "hint" }, "· 500행씩 묶어 넣습니다 · 커밋하지 않습니다")),
    h("div", { class: "imp-progress" }, bar), status, errBox,
    h("div", { class: "modal-buttons" }, closeBtn, stopBtn, goBtn));
  closeBtn.onclick = () => dlg.close();
  dlg.addEventListener("cancel", (e) => { if (running) e.preventDefault(); });
  dlg.addEventListener("close", () => dlg.remove());
  document.body.append(dlg);
  dlg.showModal();
  renderMap(true);
}
