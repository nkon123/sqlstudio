// 화면 상태(localStorage — 테마·바인드 이력·배치 …)를 데이터 폴더의 ui-state.json 에도 적는다.
//
// WebView 데이터 폴더는 로컬 PC 에 둔다 (네트워크 드라이브에 두면 첫 화면을 받는 데 몇 초씩 걸렸다).
// 리모트 환경처럼 PC 가 매번 바뀌어도 상태가 남도록, 바뀔 때마다 파일에 적고
// 창을 열 때 앱 본체가 파일이 더 새것이면 localStorage 를 그 내용으로 채운다 (lib.rs ui_state_script).
// 화면 코드는 지금처럼 localStorage 를 쓰면 된다.

import { invoke } from "@tauri-apps/api/core";

/** 마지막으로 파일과 맞춘 시각 — 파일 쪽이 더 크면 창을 열 때 파일 내용으로 바꾼다 */
const TS = "__ui_ts";
const ls = window.localStorage;
const proto = Storage.prototype;
const setItem = proto.setItem;
let timer = 0;
let saving: Promise<void> = Promise.resolve();

function save(): Promise<void> {
  timer = 0;
  const items: Record<string, string> = {};
  for (let i = 0; i < ls.length; i++) {
    const k = ls.key(i);
    if (k != null && k !== TS) items[k] = ls.getItem(k) ?? "";
  }
  saving = invoke<number>("ui_state_save", { ts: Date.now(), items })
    .then((ts) => setItem.call(ls, TS, String(ts)))
    .catch(() => {});
  return saving;
}

function schedule() {
  if (!timer) timer = window.setTimeout(() => void save(), 300);
}

/** 미뤄 둔 저장을 바로 (창을 닫기 전에) */
export function flushUiState(): Promise<void> {
  if (!timer) return saving;
  window.clearTimeout(timer);
  return save();
}

for (const name of ["setItem", "removeItem", "clear"] as const) {
  const orig = proto[name] as (this: Storage, ...a: unknown[]) => void;
  (proto as unknown as Record<string, unknown>)[name] = function (this: Storage, ...a: unknown[]) {
    orig.apply(this, a);
    if (this === ls && a[0] !== TS) schedule();
  };
}
window.addEventListener("pagehide", () => void flushUiState());
// 처음 한 번 — 예전(데이터 폴더 안) WebView 폴더에 있던 값을 파일로 옮겨 적는다
schedule();
