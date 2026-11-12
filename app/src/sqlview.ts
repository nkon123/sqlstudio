// 읽기 전용 SQL 보기 — 스크립트(DDL·GRANT)·객체 소스. 편집기와 같은 구문 강조, 줄 번호, 고르고 복사, Ctrl+F 찾기.

import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { EditorState } from "@codemirror/state";
import { EditorView, drawSelection, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap } from "@codemirror/commands";
import { oracleHighlight, oracleSql } from "./oracle-dialect";

export interface SqlView {
  el: HTMLElement;
  view: EditorView;
  /** 글을 바꾼다 (맨 위로) */
  set: (text: string) => void;
}

export function sqlView(text = "", cls = ""): SqlView {
  const el = document.createElement("div");
  el.className = `sql-view${cls ? ` ${cls}` : ""}`;
  const view = new EditorView({
    parent: el,
    state: EditorState.create({
      doc: text,
      extensions: [
        lineNumbers(),
        drawSelection(),
        EditorState.readOnly.of(true),
        // 고르기·복사·찾기는 되게 편집만 막는다 (editable 을 끄면 커서가 없어 키보드로 고를 수 없다)
        oracleSql(),
        syntaxHighlighting(oracleHighlight),
        syntaxHighlighting(defaultHighlightStyle),
        highlightSelectionMatches(),
        search({ top: true }),
        keymap.of([...searchKeymap, ...defaultKeymap]),
        EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { fontFamily: "var(--mono)" } }),
      ],
    }),
  });
  return {
    el,
    view,
    set: (t: string) => {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: t }, selection: { anchor: 0 }, scrollIntoView: true });
    },
  };
}
