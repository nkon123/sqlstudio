// npm test — 접속 그룹 나무·색
import { colorOf, countIn, groupLabel, groupTree, withGroup } from "./conngroups";
import type { Group, Profile } from "./api";

const groups: Group[] = [{ path: "ERP", color: "#f00" }, { path: "MES" }, { path: "ERP/운영" }];
const p = (name: string, group?: string, color?: string): Profile => ({ name, user: "u", connect_string: "x", group, color });
const ps = [p("a", "ERP/운영"), p("b"), p("c", "erp"), p("d", "MES", "#0f0"), p("e", "없는그룹")];
const { roots, loose } = groupTree(groups, ps);
const check = (ok: boolean, what: string) => { if (!ok) throw new Error(what); };
check(roots.map((n) => n.group.path).join() === "ERP,MES", "맨 위 그룹 순서");
check(roots[0].children.map((n) => n.group.path).join() === "ERP/운영", "하위 그룹");
check(roots[0].profiles.map((x) => x.name).join() === "c", "그룹 이름은 대소문자를 가리지 않는다");
check(countIn(roots[0]) === 2, "하위 그룹 접속까지 센다");
check(loose.map((x) => x.name).join() === "b,e", "그룹 없는 접속 (목록에 없는 그룹도)");
check(colorOf(ps[0], groups) === "#f00", "위 그룹 색을 이어받는다");
check(colorOf(ps[3], groups) === "#0f0", "접속 색이 먼저");
check(colorOf(ps[1], groups) === null, "색 없음");
check(groupLabel("ERP/운영") === "ERP / 운영" && withGroup(ps[0]) === "ERP / 운영 › a", "표시 이름");
console.log("  접속 그룹 9건");
