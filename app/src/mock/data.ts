// 가짜 모드의 테이블들 — DB 없이 화면을 시험하려고. 값은 정해진 규칙으로 만든다 (매번 같다).

import type { ColumnDesc, TableDesc } from "../api";

interface MockTable {
  desc: TableDesc;
  /** 행 수 — 많으면 스크롤로 다음 페이지를 가져오는지 본다 */
  count: number;
  /** i 번째 행 (0부터) */
  row: (i: number) => (string | null)[];
}

const c = (name: string, data_type: string, comment: string | null = null, nullable = true, def: string | null = null): ColumnDesc =>
  ({ name, data_type, nullable, default: def, comment });

const JOBS = ["CLERK", "SALESMAN", "MANAGER", "ANALYST", "PRESIDENT"];
const NAMES = ["SMITH", "ALLEN", "WARD", "JONES", "MARTIN", "BLAKE", "CLARK", "SCOTT", "KING", "TURNER", "ADAMS", "JAMES", "FORD", "MILLER"];
const STATUS = ["접수", "결제완료", "배송중", "배송완료", "취소"];
const day = (i: number) => new Date(Date.UTC(2026, 0, 1) + i * 86_400_000 * 0.37).toISOString().slice(0, 19).replace("T", " ");

export const TABLES: Record<string, MockTable> = {
  EMP: {
    desc: {
      owner: "SCOTT", name: "EMP", object_type: "TABLE", comment: "사원", num_rows: "14", last_analyzed: "2026-10-01 03:00:00",
      columns: [
        c("EMPNO", "NUMBER(4)", "사번", false), c("ENAME", "VARCHAR2(10)", "이름"), c("JOB", "VARCHAR2(9)", "직무"),
        c("MGR", "NUMBER(4)", "관리자 사번"), c("HIREDATE", "DATE", "입사일"), c("SAL", "NUMBER(7,2)", "급여"),
        c("COMM", "NUMBER(7,2)", "수당"), c("DEPTNO", "NUMBER(2)", "부서 번호"),
      ],
      primary_key: ["EMPNO"],
      indexes: [{ name: "PK_EMP", unique: true, columns: ["EMPNO"] }, { name: "IX_EMP_DEPT", unique: false, columns: ["DEPTNO"] }],
    },
    count: 14,
    row: (i) => [String(7369 + i * 50), NAMES[i % NAMES.length], JOBS[i % JOBS.length], i ? String(7839) : null,
      day(i * 40).slice(0, 10) + " 00:00:00", String(800 + ((i * 337) % 4200)), i % 3 ? null : String((i * 70) % 1400), String(10 * (1 + (i % 3)))],
  },
  DEPT: {
    desc: {
      owner: "SCOTT", name: "DEPT", object_type: "TABLE", comment: "부서", num_rows: "4",
      columns: [c("DEPTNO", "NUMBER(2)", "부서 번호", false), c("DNAME", "VARCHAR2(14)", "부서 이름"), c("LOC", "VARCHAR2(13)", "위치")],
      primary_key: ["DEPTNO"],
      indexes: [{ name: "PK_DEPT", unique: true, columns: ["DEPTNO"] }],
    },
    count: 4,
    row: (i) => [String(10 * (i + 1)), ["ACCOUNTING", "RESEARCH", "SALES", "OPERATIONS"][i], ["NEW YORK", "DALLAS", "CHICAGO", "BOSTON"][i]],
  },
  ORDERS: {
    desc: {
      owner: "SCOTT", name: "ORDERS", object_type: "TABLE", comment: "주문 (열이 많고 행이 많은 테이블)", num_rows: "1234",
      columns: [
        c("ORDER_ID", "NUMBER(12)", "주문 번호", false), c("CUST_ID", "NUMBER(10)", "고객 번호", false), c("ORD_DT", "DATE", "주문일", false, "SYSDATE"),
        c("STATUS", "VARCHAR2(20)", "주문 상태", false, "'접수'"), c("AMOUNT", "NUMBER(12,2)", "주문 금액"), c("DISCOUNT", "NUMBER(12,2)", "할인 금액", true, "0"),
        c("PAY_METHOD", "VARCHAR2(10)", "결제 수단"), c("SHIP_ZIP", "VARCHAR2(6)", "배송 우편번호"), c("SHIP_ADDR", "VARCHAR2(200)", "배송 주소"),
        c("MEMO", "VARCHAR2(400)", "요청 사항"), c("CREATED_BY", "VARCHAR2(30)", "만든 사람"), c("CREATED_AT", "TIMESTAMP(6)", "만든 시각"),
        c("UPDATED_BY", "VARCHAR2(30)", "고친 사람"), c("UPDATED_AT", "TIMESTAMP(6)", "고친 시각"),
      ],
      primary_key: ["ORDER_ID"],
      indexes: [
        { name: "PK_ORDERS", unique: true, columns: ["ORDER_ID"] },
        { name: "IX_ORDERS_CUST", unique: false, columns: ["CUST_ID", "ORD_DT"] },
        { name: "IX_ORDERS_STATUS", unique: false, columns: ["STATUS"] },
      ],
    },
    count: 1234,
    row: (i) => [String(100000 + i), String(5000 + ((i * 7919) % 900)), day(i).slice(0, 10) + " 00:00:00", STATUS[(i * 3) % STATUS.length],
      String(((i * 1531) % 500000) / 100 + 1000), i % 4 ? "0" : "1000", ["CARD", "BANK", "POINT"][i % 3], String(10000 + ((i * 97) % 89999)).padStart(5, "0"),
      `서울시 어딘가 ${i % 300}번길 ${i % 17}`, i % 5 ? null : "문 앞에 놓아 주세요", "APP", day(i) + ".000000", i % 7 ? null : "ADMIN", i % 7 ? null : day(i + 3) + ".000000"],
  },
  ORDER_ITEMS: {
    desc: {
      owner: "SCOTT", name: "ORDER_ITEMS", object_type: "TABLE", comment: "주문 품목", num_rows: "3000",
      columns: [
        c("ORDER_ID", "NUMBER(12)", "주문 번호", false), c("LINE_NO", "NUMBER(4)", "줄 번호", false), c("ITEM_ID", "NUMBER(10)", "상품 번호", false),
        c("QTY", "NUMBER(6)", "수량", false, "1"), c("PRICE", "NUMBER(12,2)", "단가"),
      ],
      primary_key: ["ORDER_ID", "LINE_NO"],
      indexes: [{ name: "PK_ORDER_ITEMS", unique: true, columns: ["ORDER_ID", "LINE_NO"] }],
    },
    count: 3000,
    row: (i) => [String(100000 + Math.floor(i / 3)), String((i % 3) + 1), String(200 + ((i * 31) % 500)), String(1 + (i % 4)), String(990 + ((i * 113) % 9000))],
  },
};

/** 목록에만 보이는 객체 (뷰·프로시저 등) */
export const OTHER_OBJECTS = [
  { owner: "SCOTT", name: "V_EMP_DEPT", object_type: "VIEW" },
  { owner: "SCOTT", name: "PKG_ORDER", object_type: "PACKAGE" },
  { owner: "SCOTT", name: "PKG_ORDER", object_type: "PACKAGE BODY" },
  { owner: "SCOTT", name: "P_CLOSE_DAY", object_type: "PROCEDURE" },
  { owner: "SCOTT", name: "SEQ_ORDER", object_type: "SEQUENCE" },
];

/** "SCOTT"."EMP" · scott.emp · emp → 테이블 */
export function findTable(name: string): MockTable | undefined {
  const last = name.replace(/"/g, "").split(".").pop()?.trim().toUpperCase() ?? "";
  return TABLES[last];
}

export function ddlOf(t: TableDesc): string {
  const cols = t.columns.map((x) => `  ${x.name.padEnd(12)} ${x.data_type}${x.default ? ` DEFAULT ${x.default}` : ""}${x.nullable ? "" : " NOT NULL"}`);
  const pk = t.primary_key.length ? [`  CONSTRAINT PK_${t.name} PRIMARY KEY (${t.primary_key.join(", ")})`] : [];
  const comments = t.columns.filter((x) => x.comment).map((x) => `COMMENT ON COLUMN ${t.owner}.${t.name}.${x.name} IS '${x.comment}';`);
  return [`CREATE TABLE ${t.owner}.${t.name} (`, [...cols, ...pk].join(",\n"), ");", "", ...comments].join("\n");
}
