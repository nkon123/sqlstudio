// 탭을 끌어 순서 바꾸기 — HTML5 드래그는 창의 파일 드롭과 얽히므로 마우스 이벤트로.
// 끄는 동안: 원래 탭은 제자리에서 흐리게, 탭의 복제본이 마우스를 따라다니고, 놓일 자리에 파란 선이 보인다.
// 놓으면 그 자리로, Esc 는 취소.

export interface TabDragHooks {
  /** 순서가 바뀐 뒤 (화면의 순서를 목록에 반영) */
  onReorder: () => void;
  /** 끌고 난 직후의 클릭은 탭을 고르는 것이 아니다 — 켜졌다가 곧 꺼진다 */
  setJustDragged: (v: boolean) => void;
}

export function enableTabDrag(el: HTMLElement, strip: HTMLElement, hooks: TabDragHooks) {
  el.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".tab-close")) return;
    const startX = e.clientX;
    let dragging = false;
    let ghost: HTMLElement | null = null;
    let mark: HTMLElement | null = null;
    let before: HTMLElement | null = null; // 놓을 자리 — 이 탭의 앞 (null 이면 맨 끝)
    let grab = 0; // 복제본을 잡은 위치 (탭 왼쪽 끝에서)
    let scroller = 0;
    let lastX = e.clientX;
    const others = () => [...strip.querySelectorAll<HTMLElement>(".tab")].filter((x) => x !== el);
    const place = () => {
      // 마우스가 다른 탭의 가운데선을 넘으면 그 앞·뒤
      const list = others();
      before = list.find((o) => { const r = o.getBoundingClientRect(); return lastX < r.left + r.width / 2; }) ?? null;
      const box = strip.getBoundingClientRect();
      const last = list[list.length - 1];
      const x = before ? before.getBoundingClientRect().left : last ? last.getBoundingClientRect().right : box.left;
      if (mark) {
        mark.style.left = `${Math.min(Math.max(x, box.left), box.right) - 1}px`;
        mark.style.top = `${box.top}px`;
        mark.style.height = `${box.height}px`;
      }
    };
    const move = (ev: MouseEvent) => {
      lastX = ev.clientX;
      if (!dragging && Math.abs(ev.clientX - startX) < 6) return;
      if (!dragging) {
        dragging = true;
        const r = el.getBoundingClientRect();
        grab = startX - r.left;
        el.classList.add("drag-origin");
        document.body.classList.add("tab-dragging");
        ghost = el.cloneNode(true) as HTMLElement;
        ghost.classList.remove("drag-origin", "active");
        ghost.classList.add("tab-ghost");
        ghost.style.width = `${r.width}px`;
        ghost.style.top = `${r.top}px`;
        document.body.append(ghost);
        mark = document.createElement("div");
        mark.className = "tab-drop";
        document.body.append(mark);
        // 가장자리에 가면 탭 줄을 스크롤
        scroller = window.setInterval(() => {
          const s = strip.getBoundingClientRect();
          if (lastX < s.left + 30) strip.scrollLeft -= 14;
          else if (lastX > s.right - 30) strip.scrollLeft += 14;
          place();
        }, 30);
      }
      if (ghost) ghost.style.left = `${ev.clientX - grab}px`;
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
