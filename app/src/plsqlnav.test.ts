// npm test — PL/SQL 구조 읽기
import { navOutline, navPath, parseNav, type NavNode } from "./plsqlnav";

let n = 0;
function eq(got: unknown, want: unknown, what: string) {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}\n  got  ${JSON.stringify(got)}\n  want ${JSON.stringify(want)}`);
  n++;
}
/** 나무를 "종류 이름[자식…]" 글로 */
const show = (xs: NavNode[]): string => xs.map((x) => `${x.kind}${x.decl ? "?" : ""} ${x.name}${x.children.length ? `[${show(x.children)}]` : ""}`).join(", ");

const BODY = `CREATE OR REPLACE PACKAGE BODY app.pkg_order AS
  /* 머리말 /* 메모 */
  CURSOR c_ord(p NUMBER) IS SELECT * FROM ord WHERE id = p FOR UPDATE;
  TYPE t_ids IS TABLE OF NUMBER INDEX BY PLS_INTEGER;
  v_txt VARCHAR2(100) := 'END; IF -- BEGIN';

  FUNCTION fn_calc(a IN NUMBER) RETURN NUMBER IS
  BEGIN
    RETURN CASE WHEN a > 0 THEN a ELSE 0 END;
  END fn_calc;

  PROCEDURE proc_settle(p_id IN NUMBER) IS
    v NUMBER;
  BEGIN
    FOR r IN (SELECT id FROM ord) LOOP
      IF r.id = p_id THEN
        v := fn_calc(r.id);
      ELSIF r.id > 0 THEN
        NULL;
      END IF;
    END LOOP;
    BEGIN
      UPDATE ord SET st = 'Y' WHERE id = p_id;
    EXCEPTION WHEN OTHERS THEN NULL;
    END;
  END proc_settle;
END pkg_order;
/
`;
const tree = parseNav(BODY);
eq(show(tree), "package body app.pkg_order[cursor? c_ord, type? t_ids, function fn_calc[case CASE], procedure proc_settle[loop FOR LOOP[sql SELECT ord, if IF], begin BEGIN[sql UPDATE ord]]]", "패키지 본문");
const settle = tree[0].children[3];
eq(BODY.slice(settle.end, settle.to), "END proc_settle;", "프로시저 끝");
eq(BODY.slice(tree[0].end, tree[0].to), "END pkg_order;", "패키지 끝");
const loop = settle.children[0];
eq(BODY.slice(loop.from, loop.from + 5), "FOR r", "LOOP 는 FOR 부터");
eq(BODY.slice(loop.end, loop.to), "END LOOP;", "END LOOP");
// 커서 위치의 길
const at = BODY.indexOf("v := fn_calc");
eq(navPath(tree, at).map((x) => x.name), ["app.pkg_order", "proc_settle", "FOR LOOP", "IF"], "현재 위치");
eq(navOutline(tree).map((x) => `${x.depth}${x.node.name}`), ["0app.pkg_order", "1c_ord", "1t_ids", "1fn_calc", "1proc_settle"], "구조 보기");

// 패키지 명세 — 선언만
const SPEC = `CREATE OR REPLACE PACKAGE pkg_order AUTHID DEFINER IS
  TYPE r_ord IS RECORD (id NUMBER, nm VARCHAR2(10));
  TYPE rc IS REF CURSOR;
  PROCEDURE proc_settle(p_id IN NUMBER);
  FUNCTION fn_calc(a IN NUMBER) RETURN NUMBER;
END;
/`;
eq(show(parseNav(SPEC)), "package pkg_order[type? r_ord, type? rc, procedure? proc_settle, function? fn_calc]", "패키지 명세");

// 익명 블록 · 트리거 · 스크립트
eq(show(parseNav("DECLARE v NUMBER; BEGIN WHILE v < 3 LOOP v := v + 1; END LOOP; END;\n/\nSELECT 1 FROM dual;")), "declare DECLARE[loop WHILE LOOP]", "익명 블록");
eq(show(parseNav("CREATE OR REPLACE TRIGGER trg_a BEFORE INSERT ON a FOR EACH ROW DECLARE x NUMBER; BEGIN IF :new.id IS NULL THEN :new.id := 1; END IF; END;\n/")), "trigger trg_a[if IF]", "트리거");
eq(show(parseNav("DROP PROCEDURE p_old;\nGRANT EXECUTE ON pkg_a TO u;\nALTER PACKAGE pkg_a COMPILE BODY;")), "", "정의가 아닌 문장");
// 닫히지 않은 블록도 끝까지 읽는다
eq(show(parseNav("CREATE PROCEDURE p IS BEGIN IF x THEN NULL;")), "procedure p[if IF]", "닫히지 않음");
// 23ai IF EXISTS 는 블록이 아니다
eq(show(parseNav("DROP TABLE IF EXISTS t;\nBEGIN NULL; END;")), "begin BEGIN", "IF EXISTS");

console.log(`  PL/SQL 구조 ${n}건`);
