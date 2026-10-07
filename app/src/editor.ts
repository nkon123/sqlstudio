// SQL 에디터 — CodeMirror 6 + Oracle PL/SQL 문법.
//
// 단축키는 Toad 에 맞춘다:
//   Ctrl+Enter / F9  커서 위치 문장 실행
//   F5               스크립트 전체 실행
//   Ctrl+E           실행계획
//   Ctrl+S           저장

import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap, startCompletion, type CompletionSource } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab, toggleComment } from "@codemirror/commands";
import { PLSQL, sql } from "@codemirror/lang-sql";
import { bracketMatching, indentOnInput, syntaxHighlighting, defaultHighlightStyle } from "@codemirror/language";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { EditorSelection, EditorState, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration, type DecorationSet, EditorView, drawSelection, highlightActiveLine, highlightActiveLineGutter,
  keymap, lineNumbers, rectangularSelection,
} from "@codemirror/view";

export interface EditorActions {
  runStatement: () => void;
  runScript: () => void;
  explain: () => void;
  save: () => void;
}

// 오류 위치 / 실행 중 문장 강조
const setMarks = StateEffect.define<{ from: number; to: number; cls: string }[]>();
const clearRunning = StateEffect.define<null>();
const marks = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(clearRunning)) {
        deco = deco.update({ filter: (_f, _t, v) => v.spec.class !== "cm-running" });
      }
      if (e.is(setMarks)) {
        deco = Decoration.set(
          e.value.filter((m) => m.to > m.from).map((m) => Decoration.mark({ class: m.cls }).range(m.from, m.to)),
          true,
        );
      }
    }
    // 고치기 시작하면 오류 표시는 지운다
    if (tr.docChanged) deco = Decoration.none;
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export class SqlEditor {
  readonly view: EditorView;

  constructor(parent: HTMLElement, actions: EditorActions, doc = "", complete?: CompletionSource) {
    const run = (f: () => void) => () => { f(); return true; };
    this.view = new EditorView({
      parent,
      state: EditorState.create({
        doc,
        extensions: [
          lineNumbers(),
          highlightActiveLineGutter(),
          history(),
          drawSelection(),
          rectangularSelection(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          // 후보는 백엔드가 구절을 보고 만든다 (언어 기본 자동완성은 쓰지 않는다)
          autocompletion({
            override: complete ? [complete] : [],
            activateOnTyping: true,
            activateOnTypingDelay: 20,
            maxRenderedOptions: 100,
            closeOnBlur: true,
          }),
          sql({ dialect: PLSQL, upperCaseKeywords: true }),
          marks,
          keymap.of([
            { key: "Ctrl-Enter", mac: "Cmd-Enter", run: run(actions.runStatement), preventDefault: true },
            { key: "F9", run: run(actions.runStatement), preventDefault: true },
            { key: "F5", run: run(actions.runScript), preventDefault: true },
            { key: "Ctrl-e", mac: "Cmd-e", run: run(actions.explain), preventDefault: true },
            { key: "Ctrl-s", mac: "Cmd-s", run: run(actions.save), preventDefault: true },
            { key: "Ctrl-/", mac: "Cmd-/", run: toggleComment },
            { key: "Ctrl-Space", run: startCompletion },
            ...closeBracketsKeymap,
            ...completionKeymap,
            ...searchKeymap,
            ...historyKeymap,
            ...defaultKeymap,
            indentWithTab,
          ]),
          EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { fontFamily: "var(--mono)" } }),
        ],
      }),
    });
  }

  text() {
    return this.view.state.doc.toString();
  }

  setText(t: string) {
    this.view.dispatch({ changes: { from: 0, to: this.view.state.doc.length, insert: t } });
  }

  cursor() {
    return this.view.state.selection.main.head;
  }

  selection(): string {
    const r = this.view.state.selection.main;
    return r.empty ? "" : this.view.state.sliceDoc(r.from, r.to);
  }

  /** 커서 위치에 넣는다 (AI 답, 객체 이름) */
  insert(t: string) {
    const r = this.view.state.selection.main;
    this.view.dispatch({
      changes: { from: r.from, to: r.to, insert: t },
      selection: EditorSelection.cursor(r.from + t.length),
    });
    this.view.focus();
  }

  /** 지금 실행하는 문장을 잠깐 칠한다 */
  flash(from: number, to: number) {
    this.view.dispatch({ effects: setMarks.of([{ from, to, cls: "cm-running" }]) });
  }

  /** ORA 오류 위치에 빨간 밑줄을 긋고 커서를 옮긴다 */
  markError(pos: number) {
    const doc = this.view.state.doc;
    const p = Math.min(Math.max(0, pos), doc.length);
    const line = doc.lineAt(p);
    // 오류 위치부터 그 단어 끝까지
    let end = p;
    while (end < line.to && /[\w$#"]/.test(doc.sliceString(end, end + 1))) end++;
    if (end === p) end = Math.min(line.to, p + 1);
    this.view.dispatch({
      effects: setMarks.of([{ from: p, to: end, cls: "cm-ora-error" }]),
      selection: EditorSelection.cursor(p),
      scrollIntoView: true,
    });
    this.view.focus();
  }

  clearMarks() {
    this.view.dispatch({ effects: setMarks.of([]) });
  }

  /** 실행 중 표시만 지운다 (오류 표시는 남긴다) */
  clearRunning() {
    this.view.dispatch({ effects: clearRunning.of(null) });
  }

  focus() {
    this.view.focus();
  }
}
