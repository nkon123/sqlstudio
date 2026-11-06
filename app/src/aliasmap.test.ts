// npm test — 문장 안 별칭 찾기 (F4 · 자동완성)
import { aliasMap } from "./aliasmap";

const check = (ok: boolean, what: string, got?: unknown) => { if (!ok) throw new Error(`${what}: ${JSON.stringify(got)}`); };
const of = (sql: string) => Object.fromEntries(aliasMap(sql));

check(of("select o.id from orders o")["O"] === "ORDERS", "같은 줄 별칭", of("select o.id from orders o"));
check(of("select * from orders\n  o where o.id = 1")["O"] === "ORDERS", "다음 줄 별칭", of("select * from orders\n  o"));
check(of("select * from a x, b as y")["Y"] === "B", "쉼표 뒤 별칭", of("select * from a x, b as y"));
check(!("SP_PROCEDURE" in of("select * from tab\n\nsp_procedure")), "빈 줄 너머는 별칭이 아니다", of("select * from tab\n\nsp_procedure"));
check(!("SP_PROCEDURE" in of("select * from tab\r\n\r\nsp_procedure")), "빈 줄 너머 (CRLF)", of("select * from tab\r\n\r\nsp_procedure"));
check(!("Z" in of("select * from a x, b\n\nz")), "쉼표 뒤도 빈 줄 너머는 아니다", of("select * from a x, b\n\nz"));
console.log("  별칭 6건");
