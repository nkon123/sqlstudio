// npm test — 왼쪽 '열린 문서' 묶기
import { dateBucket, docGroupKey, sortGroupKeys } from "./docgroups";

let n = 0;
const eq = (got: unknown, want: unknown, what: string) => {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}\n--- 나온 것\n${JSON.stringify(got)}\n--- 원하는 것\n${JSON.stringify(want)}`);
  n++;
};

// 날짜는 달력 날로 — 어젯밤 11시는 24시간이 안 지났어도 '어제'
const now = new Date(2026, 10, 4, 9, 0).getTime();
eq(dateBucket(new Date(2026, 10, 4, 0, 5).getTime(), now), "오늘", "오늘 새벽");
eq(dateBucket(new Date(2026, 10, 3, 23, 0).getTime(), now), "어제", "어젯밤");
eq(dateBucket(new Date(2026, 10, 1).getTime(), now), "지난 7일", "사흘 전");
eq(dateBucket(new Date(2026, 9, 20).getTime(), now), "지난 30일", "보름 전");
eq(dateBucket(new Date(2026, 5, 1).getTime(), now), "이전", "다섯 달 전");
eq(dateBucket(now + 60_000, now), "오늘", "시계가 조금 앞선 것");

// 접속 — 끊긴 탭은 마지막 접속과 같은 묶음, 한 번도 안 했으면 '접속 안 함'
const t = (o: object) => ({ used: now, tag: null, conn: null, lastProfile: null, ...o });
eq(docGroupKey(t({ conn: { profile: "ERP 개발" } }), "conn"), "ERP 개발", "접속");
eq(docGroupKey(t({ lastProfile: "ERP 개발" }), "conn"), "ERP 개발", "끊김은 같은 묶음");
eq(docGroupKey(t({}), "conn"), "접속 안 함", "접속 안 함");
eq(docGroupKey(t({ tag: "월마감" }), "tag"), "월마감", "태그");
eq(docGroupKey(t({}), "tag"), "태그 없음", "태그 없음");

// 순서 — 날짜는 가까운 것부터, 이름순이고 '없음' 은 맨 뒤
eq(sortGroupKeys(["이전", "오늘", "지난 7일"], "date"), ["오늘", "지난 7일", "이전"], "날짜 순서");
eq(sortGroupKeys(["태그 없음", "장애", "월마감"], "tag"), ["월마감", "장애", "태그 없음"], "태그 순서");
eq(sortGroupKeys(["접속 안 함", "MES", "ERP"], "conn"), ["ERP", "MES", "접속 안 함"], "접속 순서");

console.log(`  문서 묶기 ${n}건`);
