// npm test — Pro*C 구조 읽기 · PL/SQL 쿼리 단위
import { PROC_SAMPLE } from "./mock/plsql";
import { navOutline, navPath, type NavNode, parseNav } from "./plsqlnav";
import { isProC, parseProC } from "./procnav";

let n = 0;
function eq(got: unknown, want: unknown, what: string) {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}\n  got  ${JSON.stringify(got)}\n  want ${JSON.stringify(want)}`);
  n++;
}
const show = (xs: NavNode[]): string => xs.map((x) => `${x.kind}${x.decl ? "?" : ""} ${x.name}${x.children.length ? `[${show(x.children)}]` : ""}`).join(", ");

eq(isProC(PROC_SAMPLE), true, "Pro*C 로 본다");
eq(isProC("BEGIN NULL; END;\n/\nSELECT 1 FROM dual;"), false, "PL/SQL 은 아니다");
const tree = parseProC(PROC_SAMPLE);
eq(show(tree), [
  "declare section DECLARE SECTION",
  "struct struct ord_t",
  "c function db_connect[sql CONNECT, c block if]",
  "c function load_orders[cursor? c_ord, sql OPEN c_ord, c block for[sql FETCH c_ord, c block if[sql UPDATE orders]], sql CLOSE c_ord]",
  "c function settle_one[sql MERGE order_sum, sql EXECUTE (PL/SQL)]",
  "c function main[sql COMMIT]",
].join(", "), "Pro*C 나무");
const lo = tree[3];
eq(PROC_SAMPLE.slice(lo.from, lo.from + 22), "static int load_orders", "함수는 반환형부터");
eq(PROC_SAMPLE.slice(lo.end, lo.to), "}", "함수 끝");
const ex = tree[4].children[1];
eq(PROC_SAMPLE.slice(ex.to - 9, ex.to), "END-EXEC;", "EXECUTE … END-EXEC; 통째로");
const at = PROC_SAMPLE.indexOf("SET status = 'CN'");
eq(navPath(tree, at).map((x) => x.name), ["load_orders", "for", "if", "UPDATE orders"], "현재 위치");
eq(navOutline(tree).map((x) => x.node.name), ["DECLARE SECTION", "struct ord_t", "db_connect", "load_orders", "c_ord", "settle_one", "main"], "개요 (함수만)");
eq(navOutline(tree, true).filter((x) => x.node.kind === "sql").length, 8, "개요 (쿼리까지)");

// PL/SQL 의 쿼리 단위
const PL = `CREATE OR REPLACE PROCEDURE p IS
  CURSOR c IS SELECT * FROM t1;
BEGIN
  SELECT COUNT(*) INTO v FROM orders WHERE x = 1;
  IF v > 0 THEN
    UPDATE orders SET y = CASE WHEN a = 1 THEN 2 ELSE 3 END WHERE x = 1;
  END IF;
  FORALL i IN 1 .. ids.COUNT DELETE FROM order_sum WHERE id = ids(i);
  EXECUTE IMMEDIATE 'TRUNCATE TABLE tmp';
  OPEN rc FOR SELECT * FROM customers;
  FOR r IN (SELECT id FROM items) LOOP NULL; END LOOP;
  INSERT INTO log_t (msg) VALUES ('done');
END;
/`;
eq(show(parseNav(PL)), "procedure p[cursor? c, sql SELECT orders, if IF[sql UPDATE orders], sql FORALL DELETE order_sum, sql EXECUTE IMMEDIATE, sql OPEN rc FOR customers, loop FOR LOOP[sql SELECT items], sql INSERT log_t]", "PL/SQL 쿼리");
// 맨 바깥의 SQL 은 쿼리로 달지 않는다 (일반 SQL 문서는 그대로)
eq(show(parseNav("SELECT 1 FROM dual;\nUPDATE t SET a = 1;")), "", "일반 SQL 문서");

console.log(`  Pro*C · 쿼리 단위 ${n}건`);
