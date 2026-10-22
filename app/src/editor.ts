// SQL 에디터 — CodeMirror 6 + Oracle PL/SQL 문법.
//
// 단축키는 Toad 에 맞춘다:
//   Ctrl+Enter / F9  커서 위치 문장 실행
//   F5               스크립트 전체 실행
//   Ctrl+E           실행계획
//   Ctrl+S           저장 · Ctrl+Shift+S 다른 이름으로 저장
//   Ctrl+Shift+F     SQL 포맷 (선택 영역 또는 커서 문장)
//   Ctrl+]           짝 괄호로 (감싼 괄호의 끝 ↔ 처음)
//   Ctrl+Shift+]     괄호 안쪽 선택 (넓힌 뒤면 한 겹 안쪽으로) · Ctrl+Shift+[ 바깥 괄호 안쪽으로 넓히기
//   Ctrl+Alt+] [     같은 높이의 다음·이전 괄호 안쪽으로
//
// 여러 곳 편집 (열 편집):
//   Alt+드래그            사각형(열) 선택
//   Alt+Shift+방향키      키보드로 사각형 선택
//   Ctrl+클릭             커서 추가 · Ctrl+D 같은 단어 하나 더 선택 · Esc 커서 하나로

import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap, startCompletion, type CompletionSource } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab, toggleComment } from "@codemirror/commands";
import { sql } from "@codemirror/lang-sql";
import { ORACLE, oracleHighlight } from "./oracle-dialect";
import { snippetSource } from "./snippets";
import { quickMenu } from "./quickmenu";
import { gotoMatchingParen, parenHighlight, parenSelect } from "./brackets";
import { bracketMatching, indentOnInput, syntaxHighlighting, defaultHighlightStyle } from "@codemirror/language";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import type { Command } from "@codemirror/view";
import { Annotation, EditorSelection, EditorState, StateEffect, StateField } from "@codemirror/state";
import { type CaseMode, convertRange } from "./case-convert";
import { autoFormatExtension, caseWordBefore } from "./autofmt";
import {
  Decoration, type DecorationSet, EditorView, drawSelection, highlightActiveLine, highlightActiveLineGutter,
  crosshairCursor, keymap, lineNumbers, rectangularSelection,
} from "@codemirror/view";

/** Enter — 줄을 나누고, 새 줄을 지금 줄의 들여쓰기(첫 글자 자리)와 같게. 커서가 들여쓰기 안에 있으면 커서 앞까지만 */
export function newlineSameIndent(view: EditorView): boolean {
  if (view.state.readOnly) return false;
  // 자동 포맷: 줄을 바꾸기 전에 앞 낱말의 키워드 대소문자
  if (view.state.selection.ranges.length === 1 && view.state.selection.main.empty) caseWordBefore(view, view.state.selection.main.head, "\n");
  const tr = view.state.changeByRange((range) => {
    const line = view.state.doc.lineAt(range.from);
    const lead = /^[ \t]*/.exec(line.text)![0];
    const indent = lead.slice(0, Math.min(lead.length, range.from - line.from));
    const ins = "\n" + indent;
    // 공백만 있는 줄 끝에서 Enter — 남는 빈 줄에 공백을 남기지 않는다
    if (range.empty && line.text.trim() === "" && range.from === line.to && line.length > 0) {
      return { changes: { from: line.from, to: line.to, insert: ins }, range: EditorSelection.cursor(line.from + ins.length) };
    }
    return { changes: { from: range.from, to: range.to, insert: ins }, range: EditorSelection.cursor(range.from + ins.length) };
  });
  view.dispatch(view.state.update(tr, { scrollIntoView: true, userEvent: "input" }));
  return true;
}

/** 선택한 곳(없으면 커서의 낱말)의 구문을 대·소문자로. 문자열·따옴표 이름·주석·바인드·숫자는 건드리지 않는다 */
export function changeCase(mode: CaseMode): (view: EditorView) => boolean {
  return (view) => {
    const doc = view.state.doc.toString();
    const tr = view.state.changeByRange((range) => {
      let { from, to } = range;
      if (from === to) {
        const w = view.state.wordAt(from);
        if (!w) return { range };
        from = w.from;
        to = w.to;
      }
      const next = convertRange(doc, from, to, mode);
      if (next === doc.slice(from, to)) return { range };
      // 대부분 길이가 같아 선택은 그대로. 달라지는 드문 경우(ß → SS)는 바꾼 글 전체를 선택
      const same = next.length === to - from;
      return { changes: { from, to, insert: next }, range: same ? range : EditorSelection.range(from, from + next.length) };
    });
    if (tr.changes.empty) return true;
    view.dispatch(view.state.update(tr, { userEvent: "input.case" }));
    return true;
  };
}

export interface EditorActions {
  runStatement: () => void;
  runScript: () => void;
  explain: () => void;
  save: () => void;
  /** Ctrl+Shift+S — 다른 이름으로 저장 */
  saveAs: () => void;
  format: () => void;
  /** Ctrl+Alt+L — SELECT 목록을 몇 줄로 나누기 */
  wrapList: () => void;
  /** F4 — 커서의 테이블 컬럼 팝업 */
  describe: () => void;
  /** F8 — SQL 실행 이력 */
  history: () => void;
  /** 글이 바뀜 — 치기·붙여 넣기·잘라내기·되돌리기·끌어 놓기·포맷 모두 (파일을 불러와 바꾼 것은 아니다) */
  edited?: () => void;
}

/** setText 로 통째로 바꾼 것 — 편집이 아니다 (파일 열기·다시 읽기) */
const loaded = Annotation.define<boolean>();

// ── 키보드 사각형 선택 (Alt+Shift+방향키) ─────────────────
// 시작점(줄, 열)을 기억하고, 방향키로 끝점을 옮기며 줄마다 같은 열 범위를 선택한다.
// 다른 방법으로 선택이 바뀌면 시작점을 새로 잡는다.
const blockState = new WeakMap<EditorView, { aLine: number; aCol: number; hLine: number; hCol: number; sel: string }>();

function blockMove(dLine: number, dCol: number): Command {
  return (view) => {
    const st = view.state;
    const doc = st.doc;
    let b = blockState.get(view);
    if (!b || b.sel !== JSON.stringify(st.selection.toJSON())) {
      const head = st.selection.main.head;
      const line = doc.lineAt(head);
      b = { aLine: line.number, aCol: head - line.from, hLine: line.number, hCol: head - line.from, sel: "" };
    }
    b.hLine = Math.min(doc.lines, Math.max(1, b.hLine + dLine));
    b.hCol = Math.max(0, b.hCol + dCol);
    const [l0, l1] = b.aLine <= b.hLine ? [b.aLine, b.hLine] : [b.hLine, b.aLine];
    const ranges = [];
    let main = 0;
    for (let n = l0; n <= l1; n++) {
      const ln = doc.line(n);
      const a = ln.from + Math.min(b.aCol, ln.length);
      const h = ln.from + Math.min(b.hCol, ln.length);
      if (n === b.hLine) main = ranges.length;
      ranges.push(EditorSelection.range(a, h));
    }
    const sel = EditorSelection.create(ranges, main);
    view.dispatch({ selection: sel, scrollIntoView: true, userEvent: "select" });
    b.sel = JSON.stringify(view.state.selection.toJSON());
    blockState.set(view, b);
    return true;
  };
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
          EditorState.allowMultipleSelections.of(true),
          // Alt+드래그 사각형 선택, Alt 를 누르면 십자 커서
          rectangularSelection({ eventFilter: (e) => e.altKey }),
          crosshairCursor({ key: "Alt" }),
          indentOnInput(),
          bracketMatching(),
          // 커서를 감싼 ( … ) 쌍 — 괄호 옆이 아니어도
          parenHighlight(),
          parenSelect(),
          closeBrackets(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          syntaxHighlighting(oracleHighlight),
          syntaxHighlighting(defaultHighlightStyle),
          // 후보는 백엔드가 구절을 보고 만든다 (언어 기본 자동완성은 쓰지 않는다)
          autocompletion({
            // 백엔드 후보(테이블·컬럼…) + 코드 조각 (접속 전에도)
            override: complete ? [complete, snippetSource] : [snippetSource],
            activateOnTyping: true,
            activateOnTypingDelay: 20,
            maxRenderedOptions: 100,
            closeOnBlur: true,
          }),
          // 컬럼 후보에서 → — 구절·타입에 맞는 꼴(= '' · IS NULL …)
          quickMenu(),
          sql({ dialect: ORACLE, upperCaseKeywords: true }),
          marks,
          autoFormatExtension(),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !u.transactions.some((tr) => tr.annotation(loaded))) actions.edited?.();
          }),
          keymap.of([
            { key: "Ctrl-Enter", mac: "Cmd-Enter", run: run(actions.runStatement), preventDefault: true },
            // 줄바꿈: 새 줄은 지금 줄의 첫 글자와 같은 자리에서 시작 (언어 규칙으로 들여쓰기를 따로 정하지 않는다)
            { key: "Enter", run: newlineSameIndent },
            { key: "F9", run: run(actions.runStatement), preventDefault: true },
            { key: "F5", run: run(actions.runScript), preventDefault: true },
            { key: "Ctrl-e", mac: "Cmd-e", run: run(actions.explain), preventDefault: true },
            { key: "Ctrl-Shift-s", mac: "Cmd-Shift-s", run: run(actions.saveAs), preventDefault: true },
            { key: "Ctrl-s", mac: "Cmd-s", run: run(actions.save), preventDefault: true },
            { key: "Ctrl-/", mac: "Cmd-/", run: toggleComment },
            // 구문만 대·소문자로 (값·이름표는 그대로) — 기본의 '선택 다시 하기(Ctrl+Shift+U)' 보다 먼저
            { key: "Ctrl-Shift-u", mac: "Cmd-Shift-u", run: changeCase("upper"), preventDefault: true },
            { key: "Ctrl-Alt-u", mac: "Cmd-Alt-u", run: changeCase("lower"), preventDefault: true },
            { key: "Ctrl-Alt-l", mac: "Cmd-Alt-l", run: run(actions.wrapList), preventDefault: true },
            { key: "Ctrl-Shift-f", mac: "Cmd-Shift-f", run: run(actions.format), preventDefault: true },
            { key: "F4", run: run(actions.describe), preventDefault: true },
            { key: "F8", run: run(actions.history), preventDefault: true },
            { key: "Alt-Shift-ArrowUp", run: blockMove(-1, 0), preventDefault: true },
            { key: "Alt-Shift-ArrowDown", run: blockMove(1, 0), preventDefault: true },
            { key: "Alt-Shift-ArrowLeft", run: blockMove(0, -1), preventDefault: true },
            { key: "Alt-Shift-ArrowRight", run: blockMove(0, 1), preventDefault: true },
            { key: "Ctrl-Space", run: startCompletion },
            { key: "Mod-]", run: gotoMatchingParen, preventDefault: true },
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
    this.view.dispatch({ changes: { from: 0, to: this.view.state.doc.length, insert: t }, annotations: loaded.of(true) });
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

  /** n 번째 줄(1부터)로 커서를 옮기고 그 줄을 칠한다 */
  gotoLine(n: number) {
    const doc = this.view.state.doc;
    const line = doc.line(Math.min(Math.max(1, n), doc.lines));
    this.view.dispatch({
      effects: setMarks.of([{ from: line.from, to: line.to, cls: "cm-running" }]),
      selection: EditorSelection.cursor(line.from),
      scrollIntoView: true,
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
