// 데이터 흐름도 별도 창 — 메인 창과 따로 띄워 두고 본다 (모니터 두 대면 옆에).
// 프로필의 저장된 분석 결과를 읽으므로, 메인 창의 접속이 끊겨도 그대로 본다.

import { invoke } from "@tauri-apps/api/core";
import { errOf } from "./api";
import { DfdView } from "./dfd";
import type { DfdSource } from "./dfd-model";
import { h } from "./ui";

const q = new URLSearchParams(location.search);
const profile = q.get("profile") ?? "";
const root = document.getElementById("app")!;

async function start() {
  let g: DfdSource | null;
  try {
    g = await invoke<DfdSource | null>("analysis_result_for", { profile });
  } catch (e) {
    root.replaceChildren(h("div", { class: "dfd-empty" }, errOf(e).message));
    return;
  }
  if (!g || !g.nodes.length) {
    root.replaceChildren(h("div", { class: "dfd-empty" },
      h("p", {}, `${profile} 의 분석 결과가 없습니다.`),
      h("p", { class: "hint" }, "메인 창의 '분석' 화면에서 패키지·프로시저를 분석하면 흐름도를 그릴 수 있습니다.")));
    return;
  }
  const v = new DfdView(null, { profile: async () => profile });
  root.replaceChildren(v.el);
  const unit = q.get("unit");
  v.setData(g, q.get("center"), q.get("dir"), unit === "pkg" || unit === "proc" ? unit : null);
}

start();
