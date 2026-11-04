// 탭을 끌어 순서 바꾸기 — HTML5 드래그는 창의 파일 드롭과 얽히므로 마우스 이벤트로.
// 끄는 동안: 원래 탭은 제자리에서 흐리게, 탭의 복제본이 마우스를 따라다니고, 놓일 자리에 파란 선이 보인다.
// 놓으면 그 자리로, Esc 는 취소. 탭 줄이 세로(.vertical — 왼쪽 문서 목록)면 위아래로.

/** 문서 탭을 왼쪽 목록으로 (설정 → 편집기, 기본 켬). 끄면 편집기 위 가로 탭 */
export function tabsSidePref(): boolean {
  try { return localStorage.getItem("tabs.side") !== "0"; } catch { return true; }
}
export function setTabsSidePref(on: boolean) {
  try { localStorage.setItem("tabs.side", on ? "1" : "0"); } catch { /* 이번만 */ }
}

export interface TabDragHooks {
  /** 순서가 바뀐 뒤 (화면의 순서를 목록에 반영) */
  onReorder: () => void;
  /** 끌고 난 직후의 클릭은 탭을 고르는 것이 아니다 — 켜졌다가 곧 꺼진다 */
  setJustDragged: (v: boolean) => void;
}

export function enableTabDrag(el: HTMLElement, strip: HTMLElement, hooks: TabDragHooks) {
  el.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".tab-close")) return;
    // 묶어 볼 때는 순서를 묶음이 정한다 — 끌지 않는다
    if (strip.dataset.group && strip.dataset.group !== "none") return;
    const vertical = strip.classList.contains("vertical");
    // 끄는 방향의 좌표 — 가로 줄은 x, 세로 목록은 y
    const pos = (ev: MouseEvent) => (vertical ? ev.clientY : ev.clientX);
    const startX = pos(e);
    let dragging = false;
    let ghost: HTMLElement | null = null;
    let mark: HTMLElement | null = null;
    let before: HTMLElement | null = null; // 놓을 자리 — 이 탭의 앞 (null 이면 맨 끝)
    let grab = 0; // 복제본을 잡은 위치 (탭 왼쪽 끝에서)
    let scroller = 0;
    let lastX = startX;
    const others = () => [...strip.querySelectorAll<HTMLElement>(".tab")].filter((x) => x !== el);
    const place = () => {
      // 마우스가 다른 탭의 가운데선을 넘으면 그 앞·뒤
      const list = others().filter((o) => !o.hidden);
      const box = strip.getBoundingClientRect();
      const last = list[list.length - 1];
      if (vertical) {
        before = list.find((o) => { const r = o.getBoundingClientRect(); return lastX < r.top + r.height / 2; }) ?? null;
        const y = before ? before.getBoundingClientRect().top : last ? last.getBoundingClientRect().bottom : box.top;
        if (mark) {
          mark.style.top = `${Math.min(Math.max(y, box.top), box.bottom) - 1}px`;
          mark.style.left = `${box.left}px`;
          mark.style.width = `${box.width}px`;
          mark.style.height = "3px";
        }
        return;
      }
      before = list.find((o) => { const r = o.getBoundingClientRect(); return lastX < r.left + r.width / 2; }) ?? null;
      const x = before ? before.getBoundingClientRect().left : last ? last.getBoundingClientRect().right : box.left;
      if (mark) {
        mark.style.left = `${Math.min(Math.max(x, box.left), box.right) - 1}px`;
        mark.style.top = `${box.top}px`;
        mark.style.height = `${box.height}px`;
      }
    };
    const move = (ev: MouseEvent) => {
      lastX = pos(ev);
      if (!dragging && Math.abs(lastX - startX) < 6) return;
      if (!dragging) {
        dragging = true;
        const r = el.getBoundingClientRect();
        grab = startX - (vertical ? r.top : r.left);
        el.classList.add("drag-origin");
        document.body.classList.add("tab-dragging");
        ghost = el.cloneNode(true) as HTMLElement;
        ghost.classList.remove("drag-origin", "active");
        ghost.classList.add("tab-ghost");
        ghost.style.width = `${r.width}px`;
        if (vertical) ghost.style.left = `${r.left}px`; else ghost.style.top = `${r.top}px`;
        document.body.append(ghost);
        mark = document.createElement("div");
        mark.className = "tab-drop";
        document.body.append(mark);
        // 가장자리에 가면 탭 줄을 스크롤 (세로 목록은 목록 칸을)
        const sc = vertical ? (strip.parentElement ?? strip) : strip;
        scroller = window.setInterval(() => {
          const s = sc.getBoundingClientRect();
          if (vertical) {
            if (lastX < s.top + 30) sc.scrollTop -= 14;
            else if (lastX > s.bottom - 30) sc.scrollTop += 14;
          } else if (lastX < s.left + 30) sc.scrollLeft -= 14;
          else if (lastX > s.right - 30) sc.scrollLeft += 14;
          place();
        }, 30);
      }
      if (ghost) ghost.style[vertical ? "top" : "left"] = `${lastX - grab}px`;
      place();
    };
    const finish = (apply: boolean) => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("mouseup", up);
      clearInterval(scroller);
      ghost?.remove();
      mark?.remove();
      el.classList.remove("drag-origin");
      document.body.classList.remove("tab-dragging");
      if (!dragging) return;
      if (apply && before !== el.nextElementSibling) strip.insertBefore(el, before);
      hooks.onReorder();
      el.scrollIntoView({ inline: "nearest", block: "nearest" });
      hooks.setJustDragged(true);
      setTimeout(() => hooks.setJustDragged(false), 0);
    };
    const up = () => finish(true);
    const key = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") { ev.preventDefault(); ev.stopPropagation(); finish(false); }
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("keydown", key, true);
    window.addEventListener("mouseup", up);
  });
}
