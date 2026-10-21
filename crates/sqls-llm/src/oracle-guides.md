# Oracle 문법 안내 카드
#
# AI 에 물을 때 요청(질문·SQL·오류)과 관련된 카드만 골라 시스템 프롬프트에 넣는다. 작은 로컬 모델이 Oracle 문법을 틀리지 않게.
# 형식: "@@ 아이디 | 제목 | 찾을 낱말(쉼표) | 버전" 다음 줄부터 다음 @@ 까지가 본문.
#   찾을 낱말: 요청에 들어 있으면 고른다 (대소문자 무시). ORA-번호도 된다.  * 는 항상.
#   버전: "12-" 12c 이상만, "-11" 11g 이하만, 비우면 모두.
# 설정 폴더의 oracle-guides.md 에 같은 형식으로 쓰면 더해진다 (같은 아이디면 바꾼다) — 회사 규칙을 넣어 두면 된다.

@@ core | Oracle 기본 규칙 | * |
- LIMIT, TOP, AUTO_INCREMENT 는 없다. BOOLEAN 컬럼·`IF [NOT] EXISTS` 도 23ai 전에는 없다 (Oracle 문법이 아니다).
- SELECT 목록의 별칭은 같은 단계의 WHERE·GROUP BY 에서 쓸 수 없다 (ORDER BY 에서만) — 인라인 뷰로 감싼다.
- 빈 문자열 '' 는 NULL 이다. NULL 비교는 `IS NULL` / `IS NOT NULL`.
- 문자열 잇기는 `||`. 따옴표 안의 작은따옴표는 `''`.
- 상수만 SELECT 할 때는 `FROM DUAL`.
- 테이블 별칭에 AS 를 쓰지 않는다 (`FROM emp e`). 컬럼 별칭에는 AS 를 써도 된다.
- 날짜 상수는 `DATE '2026-10-01'` 또는 `TO_DATE('2026-10-01', 'YYYY-MM-DD')`. 문자열을 날짜 컬럼과 그냥 비교하지 마라.
- 이름은 따옴표 없이 쓰면 대문자로 저장된다.

@@ paging11 | 페이징·상위 N건 (11g: ROWNUM) | 페이징, 페이지, 상위, top, 처음, 몇 건, 몇건, 최근 n, rownum, limit, 10개, 10건, 100건, 일부만 | -11
ROWNUM 은 ORDER BY 보다 먼저 매겨진다. 반드시 이 순서로 쓴다:
① 안쪽 쿼리에서 JOIN·WHERE·GROUP BY·ORDER BY 를 끝낸다 ② 그것을 괄호로 감싸 별칭 t 를 붙인다 ③ 바깥에서 `WHERE ROWNUM <= n`.
ORDER BY 뒤에 WHERE 를 다시 쓸 수 없다. 상위 N 의 N 은 상수로 쓴다. `ROWNUM = 2`, `ROWNUM > 5` 는 늘 0건이다 (중간부터는 아래처럼 rn 으로).
```sql
-- 상위 10건: 정렬(·집계)은 안쪽, ROWNUM 은 바깥. GROUP BY 쿼리 안에 ROWNUM 을 넣지 않는다
SELECT t.* FROM (SELECT o.cust_id, SUM(o.amt) amt FROM orders o GROUP BY o.cust_id ORDER BY amt DESC) t WHERE ROWNUM <= 10;
-- 21~30번째 (3페이지, 10건씩)
SELECT * FROM (
  SELECT a.*, ROWNUM rn FROM (SELECT o.* FROM orders o ORDER BY o.order_dt DESC) a
   WHERE ROWNUM <= :page * :size)
 WHERE rn > (:page - 1) * :size;
```
FETCH FIRST / OFFSET 은 11g 에 없다.

@@ paging12 | 페이징·상위 N건 (12c 이상) | 페이징, 페이지, 상위, top, 처음, 몇 건, 몇건, 최근 n, rownum, limit, 10개, 10건, 100건, fetch first, offset | 12-
```sql
SELECT o.* FROM orders o ORDER BY o.order_dt DESC FETCH FIRST 10 ROWS ONLY;
SELECT o.* FROM orders o ORDER BY o.order_dt DESC OFFSET 20 ROWS FETCH NEXT 10 ROWS ONLY;
```
ORDER BY 없이 자르면 순서가 보장되지 않는다. 같은 값까지 포함하려면 `FETCH FIRST 10 ROWS WITH TIES`.

@@ topn_group | 그룹별 상위 N건·최신 1건 | 그룹별, 별로 최신, 별 최신, 마지막 1건, 최근 1건, 가장 최근, 첫 번째, 그룹마다, 고객별, 부서별, 상위 3, row_number, partition by | 
ROW_NUMBER 로 순번을 매긴 인라인 뷰에서 거른다 (WHERE 에서 분석 함수를 바로 쓸 수 없다).
인라인 뷰에 별칭(t)을 붙이고, 바깥에서는 t. 만 쓴다 — 안쪽 별칭(o, c)은 바깥에서 보이지 않는다.
```sql
SELECT t.cust_id, t.cust_nm, t.order_id, t.order_dt
  FROM (SELECT o.cust_id, c.cust_nm, o.order_id, o.order_dt,
               ROW_NUMBER() OVER (PARTITION BY o.cust_id ORDER BY o.order_dt DESC) rn
          FROM orders o JOIN customers c ON c.cust_id = o.cust_id) t
 WHERE t.rn = 1;   -- 고객별 최신 1건 (t.rn <= 3 이면 상위 3건). "최신"은 날짜로 정렬한다 (번호의 MAX 가 아니라)
```
같은 값을 함께 살리려면 RANK / DENSE_RANK. 한 컬럼만 필요하면 `MAX(col) KEEP (DENSE_RANK LAST ORDER BY dt)` 도 된다.

@@ date | 날짜 계산·기간 조건 | 날짜, 일자, 오늘, 어제, 내일, 이번 달, 지난 달, 최근, 7일, 30일, 주간, 월별, 기간, sysdate, trunc, add_months, months_between, last_day, 시간 차, 경과 | 
- 현재: `SYSDATE`(DATE, 초까지), `SYSTIMESTAMP`. 날짜만: `TRUNC(SYSDATE)`, 달 첫날: `TRUNC(SYSDATE, 'MM')`, 해 첫날: `TRUNC(SYSDATE, 'YYYY')`.
- 더하기: `날짜 + 1`(하루), `+ 1/24`(한 시간), `+ INTERVAL '30' MINUTE`. 달: `ADD_MONTHS(d, 1)`. 말일: `LAST_DAY(d)`.
- 두 날짜 차이: `d2 - d1` 은 일 수(소수). 시간은 `* 24`, 분은 `* 1440`. 개월: `MONTHS_BETWEEN(d2, d1)`.
- 기간 조건은 컬럼을 가공하지 말고 범위로 (인덱스를 쓴다):
```sql
WHERE o.order_dt >= TRUNC(SYSDATE) - 7 AND o.order_dt < TRUNC(SYSDATE) + 1   -- 최근 7일(오늘 포함)
WHERE o.order_dt >= TRUNC(SYSDATE, 'MM') AND o.order_dt < ADD_MONTHS(TRUNC(SYSDATE, 'MM'), 1)  -- 이번 달
```
`TRUNC(o.order_dt) = TRUNC(SYSDATE)` 처럼 컬럼에 함수를 씌우면 인덱스를 못 쓴다.

@@ dateformat | 날짜 형식 (TO_CHAR / TO_DATE) | to_char, to_date, 형식, 포맷, yyyy, 년월, 요일, 분기, 주차, 문자로, 날짜로, ora-01861, ora-01843, ora-01830, ora-01858, ora-01847, ora-01839 | 
형식 문자: YYYY 년, MM 월, DD 일, HH24 시(0-23), MI 분(MM 아님), SS 초, DY 요일 약자, DAY 요일, D 요일 번호(NLS 따라 다름), Q 분기, IW ISO 주차, FF 초 소수(TIMESTAMP).
```sql
TO_CHAR(SYSDATE, 'YYYY-MM-DD HH24:MI:SS')
TO_DATE(:s, 'YYYYMMDD')                 -- 문자 → 날짜, 형식을 반드시 준다
TO_CHAR(d, 'YYYY"년" MM"월"')           -- 글자는 큰따옴표
```
ORA-01861/01843/01830: 문자열과 형식이 다르다 — 값의 모양과 형식 문자를 맞춘다. 분은 MI, 24시간은 HH24.

@@ listagg | 여러 행을 한 줄로 (LISTAGG) | listagg, 한 줄로, 이어 붙, 쉼표로, 콤마로, 연결해서, 문자열로 합, 모아서, group_concat, string_agg, wm_concat | 
```sql
SELECT d.deptno, LISTAGG(e.ename, ', ') WITHIN GROUP (ORDER BY e.ename) AS names
  FROM emp e JOIN dept d ON d.deptno = e.deptno
 GROUP BY d.deptno;
```
- 11.2 부터 있다 (WITHIN GROUP 필수). GROUP_CONCAT·STRING_AGG 는 Oracle 에 없다. WM_CONCAT 은 비공식이라 쓰지 마라.
- 결과가 4000바이트를 넘으면 ORA-01489. 12.2 부터는 `LISTAGG(col, ',' ON OVERFLOW TRUNCATE '...') WITHIN GROUP (ORDER BY col)`.
  더 길게(CLOB) 모으려면 `RTRIM(XMLAGG(XMLELEMENT(x, col || ',') ORDER BY col).EXTRACT('//text()').getClobVal(), ',')`.
- 중복을 빼려면 안쪽에서 먼저 DISTINCT 한 인라인 뷰에 LISTAGG 를 쓴다 (LISTAGG(DISTINCT …) 는 19c 부터).

@@ string | 문자열 함수 | substr, instr, 자르, 잘라, 앞자리, 뒷자리, 포함, 찾기, 바꾸, replace, trim, 공백, lpad, rpad, 자릿수, 0 채우, 길이, length, 대문자, 소문자, 문자열 | 
- `SUBSTR(s, 시작, 길이)` 시작은 1부터, 음수면 뒤에서. `INSTR(s, '찾을', 시작, n번째)` 없으면 0.
- `LENGTH`(글자 수) / `LENGTHB`(바이트 수). VARCHAR2(10) 은 기본이 바이트라 한글은 3바이트(AL32UTF8).
- `REPLACE(s, '찾을', '바꿀')`, `TRANSLATE(s, 'abc', 'xyz')`, `TRIM(s)`, `LTRIM(s, '0')`, `LPAD(n, 5, '0')`.
- `UPPER`, `LOWER`, `INITCAP`. 포함 여부: `col LIKE '%' || :kw || '%'` 또는 `INSTR(col, :kw) > 0`.
- NULL 과 이으면 NULL 이 아니라 나머지가 그대로다 ('a' || NULL = 'a').

@@ regexp | 정규식 | 정규식, regexp, 패턴, 숫자만, 영문만, 형식 검사, 이메일, 전화번호 | 
```sql
WHERE REGEXP_LIKE(phone, '^01[0-9]-?[0-9]{3,4}-?[0-9]{4}$')
SELECT REGEXP_SUBSTR('a,b,c', '[^,]+', 1, 2) FROM dual;          -- 'b' (n번째 조각)
SELECT REGEXP_REPLACE(s, '[^0-9]', '') FROM t;                   -- 숫자만 남기기
SELECT REGEXP_COUNT(s, ',') + 1 FROM t;                          -- 조각 수 (11g+)
```
쉼표로 나뉜 값을 행으로: `SELECT REGEXP_SUBSTR(:s, '[^,]+', 1, LEVEL) FROM dual CONNECT BY REGEXP_SUBSTR(:s, '[^,]+', 1, LEVEL) IS NOT NULL;`

@@ null | NULL 처리 | null, 널, 빈 값, 빈값, nvl, nvl2, coalesce, 없으면 0, 0으로, 기본값으로, not in, is null, 0으로 나누, ora-01476, divisor is equal to zero | 
- `NVL(a, 0)`, `NVL2(a, 있을 때, 없을 때)`, `COALESCE(a, b, c)`(처음으로 NULL 이 아닌 값), `NULLIF(a, b)`.
- `= NULL` 은 항상 거짓이다. `IS NULL` 을 쓴다.
- `NOT IN (서브쿼리)` 에 NULL 이 하나라도 있으면 결과가 0건이 된다 → `NOT EXISTS` 를 쓴다.
- `COUNT(col)` 은 NULL 을 세지 않는다. `COUNT(*)` 는 행 수.
- 나눗셈 0 방지: `a / NULLIF(b, 0)` (0 으로 나누면 ORA-01476).

@@ case | 조건 값 (CASE / DECODE) | case, decode, 조건에 따라, 경우에 따라, 이면, 아니면, 구분값, 코드값을, 변환해서, 표시를 | 
```sql
CASE WHEN o.amt >= 100000 THEN '대' WHEN o.amt >= 10000 THEN '중' ELSE '소' END
CASE o.status WHEN 'P' THEN '결제' WHEN 'C' THEN '취소' ELSE '기타' END
DECODE(o.status, 'P', '결제', 'C', '취소', '기타')   -- 같다/아니다 비교만, NULL = NULL 로 본다
```
조건별 건수: `SUM(CASE WHEN o.status = 'C' THEN 1 ELSE 0 END)` 또는 `COUNT(CASE WHEN … THEN 1 END)`.

@@ merge | MERGE — 있으면 UPDATE, 없으면 INSERT | merge, upsert, 있으면, 없으면 insert, 없으면 추가, 동기화, 갱신 또는, 넣거나 고치, ora-38104, ora-30926 | 
```sql
MERGE INTO stock t
USING (SELECT :item_id AS item_id, :qty AS qty FROM dual) s
   ON (t.item_id = s.item_id)
 WHEN MATCHED THEN
   UPDATE SET t.qty = t.qty + s.qty, t.upd_dt = SYSDATE
   DELETE WHERE t.qty <= 0                      -- 선택
 WHEN NOT MATCHED THEN
   INSERT (item_id, qty, upd_dt) VALUES (s.item_id, s.qty, SYSDATE);
```
- ON 절에 쓴 컬럼은 UPDATE SET 에 넣을 수 없다 (ORA-38104).
- USING 쪽에 같은 키가 두 번 있으면 ORA-30926 — USING 을 키로 먼저 하나로 줄인다.

@@ hierarchy | 계층 쿼리 (트리) | 계층, 트리, 상위 부서, 하위 부서, 부모, 자식, 조직도, 메뉴, 레벨, depth, connect by, start with, prior, 경로, 재귀 | 
```sql
SELECT LEVEL, LPAD(' ', (LEVEL - 1) * 2) || d.dept_nm AS name, SYS_CONNECT_BY_PATH(d.dept_nm, '/') AS path,
       CONNECT_BY_ISLEAF AS leaf
  FROM dept d
 START WITH d.parent_id IS NULL
CONNECT BY PRIOR d.dept_id = d.parent_id     -- PRIOR 가 붙은 쪽이 부모
 ORDER SIBLINGS BY d.sort_no;
```
- 고리가 있으면 `CONNECT BY NOCYCLE`. 아래에서 위로는 `START WITH id = :id CONNECT BY PRIOR parent_id = id`.
- WHERE 는 계층을 만든 뒤에 거른다. 11.2 부터 재귀 WITH (`WITH t (id, lvl) AS (… UNION ALL …)`) 도 된다.

@@ analytic | 분석 함수 (누적·이전 행·비율) | 누적, 누계, 이전 행, 다음 행, 전월, 전일, 증감, 비율, 점유율, 순위, rank, lag, lead, over, 이동 평균, 합계 대비 | 
```sql
SUM(amt) OVER (PARTITION BY cust_id ORDER BY order_dt ROWS UNBOUNDED PRECEDING)   -- 누적 합
LAG(amt, 1, 0) OVER (PARTITION BY cust_id ORDER BY order_dt)                       -- 이전 행 값
amt - LAG(amt) OVER (ORDER BY ym)                                                  -- 전월 대비 증감
RATIO_TO_REPORT(amt) OVER (PARTITION BY ym)                                        -- 비율 (0~1)
AVG(amt) OVER (ORDER BY dt ROWS BETWEEN 6 PRECEDING AND CURRENT ROW)               -- 7일 이동 평균
RANK() OVER (ORDER BY amt DESC)   /  DENSE_RANK()  /  ROW_NUMBER()
```
분석 함수는 WHERE·GROUP BY 뒤에 계산된다. 결과로 거르려면 인라인 뷰로 감싼다.

@@ groupby | GROUP BY·집계 규칙 | group by, 그룹, 집계, 합계, 소계, 총계, 월별, 일별, having, rollup, cube, grouping, ora-00979, ora-00937, ora-00934 | 
- SELECT 에서 집계 함수 밖의 컬럼은 모두 GROUP BY 에 있어야 한다 (ORA-00979 / ORA-00937).
- 집계 조건은 WHERE 가 아니라 HAVING (ORA-00934).
- 월별: `GROUP BY TRUNC(order_dt, 'MM')` 또는 `TO_CHAR(order_dt, 'YYYY-MM')` — SELECT 에도 같은 식.
- 소계·총계: `GROUP BY ROLLUP(ym, cust_id)`, 소계 행 구분은 `GROUPING(cust_id) = 1`. 원하는 조합만: `GROUPING SETS ((ym), (cust_id), ())`.

@@ pivot | 행을 열로 (PIVOT) | pivot, unpivot, 행을 열로, 열을 행으로, 가로로, 세로로, 크로스탭, 월별로 펼, 교차 | 
```sql
SELECT * FROM (SELECT cust_id, TO_CHAR(order_dt, 'MM') mm, amt FROM orders)
 PIVOT (SUM(amt) FOR mm IN ('01' AS m01, '02' AS m02, '03' AS m03));
```
- IN 목록은 상수여야 한다 (동적이면 동적 SQL). 11g 부터 있다.
- 같은 결과를 CASE 로: `SUM(CASE WHEN mm = '01' THEN amt END) AS m01`.

@@ join | 조인 | join, 조인, 외부 조인, outer join, left join, 없는 것도, 매칭 안 되는, (+), exists, in 서브쿼리, 세미 조인, 안티 조인, 있는지 확인, 존재 여부, 없는 것만 | 
- ANSI 조인을 쓴다: `FROM a LEFT JOIN b ON b.id = a.id AND b.use_yn = 'Y'` — 바깥쪽 테이블의 조건을 WHERE 에 쓰면 내부 조인이 된다.
- 옛 문법 `a.id = b.id(+)` 은 ANSI 와 섞지 않는다. FULL OUTER 는 ANSI 로만.
- 있는지만 볼 때는 `EXISTS (SELECT 1 FROM b WHERE b.id = a.id)`. 없는 것: `NOT EXISTS` (NOT IN 은 NULL 문제).
- 1:N 조인 후 SUM 하면 1쪽 값이 부풀려진다 — N쪽을 먼저 집계한 인라인 뷰와 조인한다.

@@ dedupe | 중복 찾기·지우기 | 중복, 같은 값, 두 번, 여러 번, 한 건만 남기, 유일, distinct, 중복 제거, rowid | 
```sql
-- 중복 찾기
SELECT cust_id, ord_no, COUNT(*) FROM orders GROUP BY cust_id, ord_no HAVING COUNT(*) > 1;
-- 하나만 남기고 지우기
DELETE FROM orders WHERE ROWID IN (
  SELECT rid FROM (SELECT ROWID rid, ROW_NUMBER() OVER (PARTITION BY cust_id, ord_no ORDER BY order_id) rn FROM orders)
   WHERE rn > 1);
```

@@ sequence | 시퀀스·자동 번호 | 시퀀스, sequence, nextval, currval, 자동 증가, 자동증가, 일련번호, 채번, auto_increment, identity | -11
- AUTO_INCREMENT·IDENTITY 는 11g 에 없다. 시퀀스를 만든다: `CREATE SEQUENCE order_seq START WITH 1 CACHE 20;`
- 넣을 때: `INSERT INTO orders (order_id, …) VALUES (order_seq.NEXTVAL, …)`. 방금 값: `order_seq.CURRVAL` (같은 세션에서 NEXTVAL 뒤에만).
- 자동으로 넣으려면 BEFORE INSERT 트리거에서 `:NEW.order_id := order_seq.NEXTVAL;`
- 방금 넣은 키를 받으려면 `INSERT … RETURNING order_id INTO :new_id` (PL/SQL).

@@ sequence12 | 시퀀스·IDENTITY (12c 이상) | 시퀀스, sequence, nextval, currval, 자동 증가, 자동증가, 일련번호, 채번, auto_increment, identity | 12-
- `order_id NUMBER GENERATED BY DEFAULT AS IDENTITY` 또는 `DEFAULT order_seq.NEXTVAL` (12c+).
- 시퀀스: `CREATE SEQUENCE order_seq START WITH 1 CACHE 20;` → `order_seq.NEXTVAL`.
- 방금 넣은 키: `INSERT … RETURNING order_id INTO :new_id`.

@@ insert | INSERT 여러 행·복사 | insert, 여러 행, 여러건, 한 번에 넣, 복사해서 넣, insert select, insert all, values 여러 | 
- `VALUES (…), (…)` 처럼 여러 행을 한 문장에 쓰는 것은 23ai 전에는 안 된다.
- 다른 테이블에서: `INSERT INTO t (a, b) SELECT x, y FROM s WHERE …;`
- 여러 행 상수: `INSERT ALL INTO t (a, b) VALUES (1, 'x') INTO t (a, b) VALUES (2, 'y') SELECT * FROM dual;`
  또는 `INSERT INTO t (a, b) SELECT 1, 'x' FROM dual UNION ALL SELECT 2, 'y' FROM dual;`
- 컬럼 목록을 항상 쓴다.

@@ update | 다른 테이블 값으로 UPDATE | update, 수정, 변경, 갱신, 다른 테이블 값으로, 조인해서 업데이트, update join, 일괄 수정 | 
`UPDATE … FROM …` / `UPDATE a JOIN b` 문법은 Oracle 에 없다.
```sql
UPDATE orders o
   SET o.cust_nm = (SELECT c.cust_nm FROM customers c WHERE c.cust_id = o.cust_id)
 WHERE EXISTS (SELECT 1 FROM customers c WHERE c.cust_id = o.cust_id);   -- 없으면 NULL 이 되지 않게
```
여러 컬럼: `SET (a, b) = (SELECT x, y FROM …)`. 대량이면 MERGE 가 빠르다.

@@ plsql | PL/SQL 블록·변수·SELECT INTO | plsql, pl/sql, 블록, begin, declare, 변수, select into, %type, %rowtype, dbms_output, 익명 블록, ora-01403, ora-01422, ora-06502 | 
```sql
DECLARE
  v_amt  orders.amt%TYPE;          -- 컬럼과 같은 형
  r_ord  orders%ROWTYPE;
BEGIN
  SELECT amt INTO v_amt FROM orders WHERE order_id = :id;
  DBMS_OUTPUT.PUT_LINE('금액: ' || v_amt);
EXCEPTION
  WHEN NO_DATA_FOUND THEN NULL;    -- 0건 (ORA-01403)
  WHEN TOO_MANY_ROWS THEN RAISE;   -- 2건 이상 (ORA-01422)
END;
/
```
- SELECT INTO 는 꼭 1건이어야 한다. 변수 길이가 모자라면 ORA-06502.
- 사용자 오류: `RAISE_APPLICATION_ERROR(-20001, '메시지');`

@@ cursor | 커서·루프 | 커서, cursor, 루프, loop, for 루프, 한 건씩, 반복, ref cursor, sys_refcursor, 결과 집합 반환 | 
```sql
FOR r IN (SELECT order_id, amt FROM orders WHERE status = 'P') LOOP
  … r.order_id …
END LOOP;
-- 결과를 돌려주는 프로시저
PROCEDURE get_orders(p_cust IN NUMBER, p_rc OUT SYS_REFCURSOR) IS
BEGIN
  OPEN p_rc FOR SELECT * FROM orders WHERE cust_id = p_cust;
END;
```
한 건씩 DML 하는 루프는 느리다 — 가능하면 한 문장(INSERT … SELECT, MERGE)으로.

@@ bulk | 대량 처리 (BULK COLLECT / FORALL) | bulk, 대량, 일괄, 많은 건, 빠르게 넣, forall, bulk collect, limit, save exceptions | 
```sql
DECLARE
  TYPE t_ids IS TABLE OF orders.order_id%TYPE;
  v_ids t_ids;
  CURSOR c IS SELECT order_id FROM orders WHERE status = 'P';
BEGIN
  OPEN c;
  LOOP
    FETCH c BULK COLLECT INTO v_ids LIMIT 1000;
    EXIT WHEN v_ids.COUNT = 0;
    FORALL i IN 1 .. v_ids.COUNT
      UPDATE orders SET status = 'S' WHERE order_id = v_ids(i);
    COMMIT;
  END LOOP;
  CLOSE c;
END;
/
```
실패 행을 모으려면 `FORALL … SAVE EXCEPTIONS` 후 `SQL%BULK_EXCEPTIONS`.

@@ procedure | 프로시저·함수·패키지 만들기 | 프로시저, procedure, 함수, function, 패키지, package, create or replace, 파라미터, 매개변수, in out, 리턴 | 
```sql
CREATE OR REPLACE PROCEDURE close_order(p_id IN NUMBER, p_cnt OUT NUMBER) IS
BEGIN
  UPDATE orders SET status = 'C' WHERE order_id = p_id;
  p_cnt := SQL%ROWCOUNT;
END close_order;
/
CREATE OR REPLACE FUNCTION order_amt(p_id IN NUMBER) RETURN NUMBER IS
  v NUMBER;
BEGIN
  SELECT amt INTO v FROM orders WHERE order_id = p_id;
  RETURN v;
END;
/
```
- 파라미터 형에 길이를 쓰지 않는다 (VARCHAR2(10) 이 아니라 VARCHAR2).
- 패키지: `CREATE OR REPLACE PACKAGE pkg AS … END;` (선언) 와 `CREATE OR REPLACE PACKAGE BODY pkg AS … END;` (본문).
- 호출: `BEGIN close_order(1, :cnt); END;` 또는 `EXEC close_order(1, :cnt)`.

@@ dynamic | 동적 SQL | 동적, dynamic, execute immediate, 문자열로 만든 sql, 테이블 이름을 변수, ddl 실행 | 
```sql
EXECUTE IMMEDIATE 'UPDATE ' || DBMS_ASSERT.SQL_OBJECT_NAME(p_tab) || ' SET status = :s WHERE id = :id'
  USING p_status, p_id;
EXECUTE IMMEDIATE 'SELECT COUNT(*) FROM orders WHERE cust_id = :c' INTO v_cnt USING p_cust;
EXECUTE IMMEDIATE 'TRUNCATE TABLE tmp_work';      -- PL/SQL 안의 DDL
```
값은 문자열로 잇지 말고 USING 바인드로 (SQL 인젝션, 파싱 낭비). 이름은 DBMS_ASSERT 로 검사.

@@ ddl | 테이블 만들기·바꾸기 (DDL) | create table, 테이블 생성, 테이블 만들, alter table, 컬럼 추가, 컬럼 변경, 컬럼 삭제, 제약, 기본 키, primary key, foreign key, comment, 주석 | 
```sql
CREATE TABLE orders (
  order_id  NUMBER(12)        NOT NULL,
  cust_id   NUMBER(10)        NOT NULL,
  memo      VARCHAR2(200 CHAR),
  order_dt  DATE DEFAULT SYSDATE NOT NULL,
  CONSTRAINT pk_orders PRIMARY KEY (order_id),
  CONSTRAINT fk_orders_cust FOREIGN KEY (cust_id) REFERENCES customers (cust_id)
);
COMMENT ON TABLE orders IS '주문';
COMMENT ON COLUMN orders.memo IS '메모';
ALTER TABLE orders ADD (status VARCHAR2(1) DEFAULT 'N');
ALTER TABLE orders MODIFY (memo VARCHAR2(400 CHAR));
ALTER TABLE orders DROP COLUMN status;
```
- 형: VARCHAR2(n CHAR)(한글은 CHAR 단위 권장), NUMBER(p,s), DATE, TIMESTAMP, CLOB. BOOLEAN·TEXT·INT AUTO_INCREMENT 없음.
- DDL 은 자동 커밋된다.

@@ conversion | 형 변환·숫자 오류 | to_number, 숫자로, 문자로, 형 변환, 형변환, 암시적, ora-01722, ora-12899, ora-01438, ora-00932, ora-12704, 자리수 초과, invalid number, inconsistent datatypes, character set mismatch | 
- ORA-01722: 숫자로 바꿀 수 없는 문자가 있다. 문자 컬럼을 숫자와 비교하면(`char_col = 123`) 모든 값을 숫자로 바꾸다 터지고 인덱스도 못 쓴다 → `char_col = '123'` 처럼 형을 맞춘다.
- 숫자 검사: `REGEXP_LIKE(s, '^-?[0-9]+(\.[0-9]+)?$')` 인 것만 TO_NUMBER.
- ORA-12899: 값이 컬럼 길이보다 길다 (한글은 바이트로 3배). ORA-01438: NUMBER 자리수 초과.
- ORA-00932: 형이 다르다 — CLOB 를 `=`·GROUP BY 로 비교, 날짜와 숫자, CASE 각 갈래의 형이 다름 → 형을 맞춘다.
- ORA-12704: VARCHAR2 와 NVARCHAR2(NCHAR) 를 UNION·CASE·NVL·DECODE 로 섞었다 → `TO_CHAR(ncol)` 또는 `TO_NCHAR(col)` 로 한쪽에 맞춘다.

@@ ora_syntax | 문법 오류 (ORA-00933·00907·00936·00923·00911) | ora-00933, ora-00907, ora-00936, ora-00923, ora-00911, ora-00905, sql command not properly ended, missing right parenthesis, missing expression | 
- ORA-00933: 문장 끝 이상 — LIMIT/TOP, 테이블 별칭에 AS, ORDER BY 뒤 이상한 절, 문장 끝의 `;` 를 실행기에 그대로 넘긴 경우.
- ORA-00907: 괄호 짝, 또는 서브쿼리 안의 ORDER BY(인라인 뷰 말고 IN/스칼라 서브쿼리), 데이터형 길이 위치 실수.
- ORA-00936: 빠진 식 — `SELECT , a`, 끝에 남은 쉼표, 빈 IN ().
- ORA-00923: FROM 이 없거나 SELECT 목록의 쉼표·따옴표 실수.
- ORA-00911: 잘못된 문자 — 끝의 `;`(동적 SQL·JDBC), 전각 문자, 스마트 따옴표.

@@ ora_name | 이름 오류 (ORA-00904·00942·00918·00960) | ora-00904, ora-00942, ora-00918, ora-00960, invalid identifier, table or view does not exist, column ambiguously defined | 
- ORA-00904: 없는 컬럼 — 철자, 따옴표로 만든 소문자 이름(`"Name"`), SELECT 별칭을 WHERE 에서 쓴 경우(같은 단계에서는 못 쓴다 → 인라인 뷰).
- ORA-00942: 테이블이 없거나 권한이 없다 — 스키마 이름(`app.orders`), 동의어, GRANT 확인.
- ORA-00918: 두 테이블에 같은 이름의 컬럼 — 별칭을 붙인다 (`o.cust_id`).

@@ ora_subquery | 서브쿼리·건수 오류 (ORA-01427 등) | ora-01427, single-row subquery returns more than one row, 서브쿼리가 여러 건, 스칼라 서브쿼리 | 
- ORA-01427: `= (SELECT …)` 서브쿼리가 2건 이상 — 조건을 더하거나 `IN`, 또는 `MAX()`/`ROWNUM = 1` 로 하나만.
- 스칼라 서브쿼리(SELECT 목록 안)도 1건 이하여야 한다. 많은 행에 쓰면 느리다 → 조인으로.

@@ ora_constraint | 제약 위반 (ORA-00001·01400·02291·02292·02290) | ora-00001, ora-01400, ora-02291, ora-02292, ora-02290, unique constraint, cannot insert null, parent key not found, child record found, 중복 키 | 
- ORA-00001: 유일 키 중복 — 이미 있는지 먼저 보거나 MERGE.
- ORA-01400: NOT NULL 컬럼에 NULL — 컬럼 목록·기본값 확인.
- ORA-02291: 부모 키가 없다 (FK) — 부모 테이블에 먼저 넣는다.
- ORA-02292: 자식 행이 있어 지울 수 없다 — 자식을 먼저 지우거나 ON DELETE CASCADE.
- ORA-02290: CHECK 제약 위반.

@@ ora_plsql | PL/SQL 실행 오류 (ORA-04091·06512·06550) | ora-04091, ora-06512, ora-06550, pls-00201, pls-00103, mutating, 변경 중인 테이블, 트리거 오류 | 
- ORA-04091: 행 트리거 안에서 자기 테이블을 조회·변경 — :NEW/:OLD 만 쓰거나, 복합 트리거(11g+)·문장 트리거로.
- ORA-06512: 오류가 난 줄의 위치 (원인은 그 위의 첫 오류 메시지).
- ORA-06550 / PLS-00201: 이름을 모른다 — 선언·권한(EXECUTE)·스키마 확인. PLS-00103: 문법 (세미콜론·END 짝).

@@ ora_lock | 잠금·오래 걸림 (ORA-00054·00060·01555) | ora-00054, ora-00060, ora-01555, resource busy, deadlock, snapshot too old, 잠금, 락, 대기 | 
- ORA-00054: 다른 세션이 잡고 있다 (NOWAIT·DDL) — 커밋 안 한 세션을 찾는다 (세션 모니터).
- ORA-00060: 교착 — 두 세션이 서로의 행을 기다린다. 같은 순서로 갱신한다.
- ORA-01555: 오래 걸리는 조회 중 UNDO 가 덮였다 — 조회를 짧게, 커서 루프 중 커밋을 줄인다.
- 조회를 잠그며 고칠 때: `SELECT … FOR UPDATE [NOWAIT | WAIT 5]`.

@@ tuning | 튜닝 기본 | 튜닝, 느려, 느린, 성능, 빠르게, 오래 걸, 인덱스, full scan, 풀스캔, 실행계획, optimize, 힌트 | 
- 컬럼에 함수·연산을 씌우면 인덱스를 못 쓴다: `TRUNC(dt) = …`, `col + 0`, `SUBSTR(col, 1, 4) = …` → 범위 조건으로 바꾼다.
- 형이 다르면 암시적 변환으로 인덱스를 못 쓴다 (문자 컬럼 = 숫자).
- `LIKE '%kw'` (앞 와일드카드) 는 인덱스 범위 검색이 안 된다.
- OR 조건이 느리면 UNION ALL 로 나누어 본다. IN (큰 서브쿼리) 보다 EXISTS / 조인.
- 스칼라 서브쿼리·함수 호출이 행마다 돌면 조인으로 바꾼다.
- 결합 인덱스는 앞 컬럼이 조건에 있어야 한다. 힌트(/*+ INDEX(t ix) */)는 마지막 수단.

@@ with | WITH 절 (공통 테이블 식) | with 절, with절, cte, 공통 테이블, 서브쿼리 팩토링, materialize, 단계별로 나눠, 중간 결과 | 
```sql
WITH s AS (SELECT o.cust_id, SUM(o.amt) amt FROM orders o GROUP BY o.cust_id),
     big AS (SELECT s.* FROM s WHERE s.amt > 100000)          -- 쉼표로 잇는다 (WITH 를 두 번 쓰지 않는다)
SELECT c.cust_nm, b.amt FROM big b JOIN customers c ON c.cust_id = b.cust_id;
INSERT INTO cust_sum (cust_id, amt) WITH s AS (SELECT …) SELECT cust_id, amt FROM s;   -- INSERT 는 INSERT INTO 뒤에 WITH
```
- WITH 는 바로 뒤 한 문장에서만 보인다. 각 이름 뒤에 `;` 를 쓰지 않는다.
- 여러 번 쓰는 무거운 결과를 한 번만 계산하게: `WITH s AS (SELECT /*+ MATERIALIZE */ …)`.

@@ setops | 집합 연산 (UNION·MINUS·INTERSECT) | union, union all, minus, intersect, except, 합집합, 차집합, 교집합, 위아래로 합, 두 결과를 합, 한쪽에만 있는, 에는 있고, 에는 없는, 는 안 한, 는 안 산, 둘 다 있는, ora-01789, ora-01790 | 
- `UNION` 은 중복을 지우느라 정렬한다 — 중복이 없거나 상관없으면 `UNION ALL`.
- 차집합은 `MINUS` (`EXCEPT` 는 21c 부터). 교집합 `INTERSECT`.
- 각 SELECT 의 컬럼 수(ORA-01789)와 형(ORA-01790)이 같아야 한다. NULL 자리는 `CAST(NULL AS VARCHAR2(10))` 처럼 형을 준다.
- ORDER BY 는 맨 끝에 한 번만 — 첫 SELECT 의 별칭이나 위치 번호(`ORDER BY 2`)로.
```sql
SELECT cust_id FROM orders WHERE order_dt >= DATE '2026-01-01'
MINUS
SELECT cust_id FROM orders WHERE order_dt < DATE '2026-01-01';   -- 올해 처음 주문한 고객
```

@@ number | 숫자 함수·형식 (반올림·천 단위) | 반올림, 올림, 버림, 내림, 소수점, 소수 자리, round, ceil, floor, mod, 나머지, 천 단위, 콤마 찍, 쉼표 찍, 금액 형식, 퍼센트, 백분율, 정수로 | 
- `ROUND(3.456, 2)` = 3.46, `TRUNC(3.456, 2)` = 3.45(버림), `CEIL`(올림)·`FLOOR`(내림), `ROUND(n, -3)` 천 단위 반올림, `MOD(10, 3)` = 1.
- 몫: `TRUNC(a / b)` (정수 나눗셈 연산자는 없다). 비율: `ROUND(a / NULLIF(b, 0) * 100, 1)`.
```sql
TO_CHAR(1234567, 'FM999,999,999')     -- '1,234,567' (FM: 앞 공백 없앰)
TO_CHAR(0.5, 'FM990.00')              -- '0.50'  (0 은 자리를 꼭 채운다, 9 는 비우면 생략)
TO_NUMBER('1,234', '9,999')           -- 쉼표가 든 문자 → 숫자
```
형식의 9 자리가 모자라면 `####` 로 나온다.

@@ delete | 삭제 (DELETE·TRUNCATE) | delete, 삭제, 지워, 지우기, 지우는, truncate, 비우, 전체 삭제, 조인해서 삭제, 대량 삭제, ora-02266 | 
- `DELETE a FROM a JOIN b` 문법은 없다 → `DELETE FROM orders o WHERE EXISTS (SELECT 1 FROM cancel_list c WHERE c.order_id = o.order_id);`
- `TRUNCATE TABLE t` 는 DDL — 바로 커밋되어 되돌릴 수 없고, WHERE 를 못 쓰고, 다른 테이블이 FK 로 가리키면 안 된다 (ORA-02266). 빠르고 공간도 돌려준다.
- 수백만 건 DELETE 는 UNDO 가 커진다 — 나눠 지우거나, 남길 것만 새 테이블로 옮긴(CTAS) 뒤 바꾼다.
- 지우기 전에 같은 WHERE 로 `SELECT COUNT(*)` 를 먼저 본다.

@@ rowgen | 행 만들기·날짜 목록 (CONNECT BY LEVEL) | 달력, 날짜 목록, 모든 날짜, 빠짐없이, 빈 날짜, 없는 날짜, 0으로 채, 1부터, 연속 번호, 번호 생성, 행 생성, 일자별 전부, connect by level | 
```sql
SELECT LEVEL n FROM dual CONNECT BY LEVEL <= 10;                         -- 1~10
SELECT TRUNC(SYSDATE, 'MM') + LEVEL - 1 dt FROM dual
CONNECT BY LEVEL <= LAST_DAY(SYSDATE) - TRUNC(SYSDATE, 'MM') + 1;        -- 이번 달 모든 날짜
-- 주문 없는 날도 0 으로 (최근 7일)
SELECT d.dt, NVL(SUM(o.amt), 0) amt
  FROM (SELECT TRUNC(SYSDATE) - LEVEL + 1 dt FROM dual CONNECT BY LEVEL <= 7) d
  LEFT JOIN orders o ON o.order_dt >= d.dt AND o.order_dt < d.dt + 1
 GROUP BY d.dt ORDER BY d.dt;
```

@@ dictionary | 데이터 사전 조회 (테이블·컬럼·소스 찾기) | 데이터 사전, 딕셔너리, 테이블 목록, 컬럼 목록, 어떤 테이블, 어느 테이블, 컬럼이 있는, 코멘트 조회, 주석 조회, 인덱스 목록, 제약 조건 목록, 메타, all_tab_columns, user_tables, all_objects, all_source, invalid 객체 | 
USER_(내 것) · ALL_(볼 수 있는 것) · DBA_(전부, 권한 필요). 이름은 대문자로 저장된다 (`table_name = 'ORDERS'`).
```sql
SELECT owner, table_name FROM all_tab_columns WHERE column_name = 'CUST_ID';            -- 이 컬럼이 있는 테이블
SELECT column_name, data_type, data_length, nullable FROM all_tab_columns
 WHERE owner = 'APP' AND table_name = 'ORDERS' ORDER BY column_id;                      -- 컬럼 목록
SELECT table_name, column_name, comments FROM all_col_comments WHERE comments LIKE '%주문%';  -- 설명으로 찾기
SELECT name, type, line, text FROM all_source WHERE UPPER(text) LIKE '%ORDERS%';        -- 소스에서 찾기
SELECT object_name, object_type FROM user_objects WHERE status = 'INVALID';             -- 깨진 객체
```
그 밖에: ALL_TAB_COMMENTS, ALL_INDEXES·ALL_IND_COLUMNS, ALL_CONSTRAINTS·ALL_CONS_COLUMNS, ALL_DEPENDENCIES, USER_ERRORS(컴파일 오류).

@@ trigger | 트리거 | 트리거, trigger, :new, :old, before insert, after update, 변경 이력, 이력 남기, 자동으로 넣, 자동으로 들어가, 자동으로 채, 수정일 자동, 수정일시, 등록일시, ora-04092, ora-04084 | 
```sql
CREATE OR REPLACE TRIGGER trg_orders_biu
BEFORE INSERT OR UPDATE ON orders
FOR EACH ROW
BEGIN
  :NEW.upd_dt := SYSDATE;
  IF INSERTING THEN :NEW.reg_dt := SYSDATE; END IF;
END;
/
```
- 값을 바꾸는 것은 BEFORE 행 트리거에서 (AFTER 에서 `:NEW` 를 바꾸면 ORA-04084). 이력은 AFTER 행 트리거에서 `INSERT INTO orders_hist … VALUES (:OLD.order_id, …)`.
- `WHEN (NEW.amt > 0)` 절에서는 콜론 없이 NEW/OLD. 트리거 안에서 COMMIT 은 안 된다 (ORA-04092).
- 자기 테이블을 조회하면 ORA-04091 (변경 중인 테이블).

@@ view | 뷰·구체화 뷰 | 뷰 만들, 뷰 생성, create view, create or replace view, 뷰를 만, materialized view, mview, 구체화 뷰, 스냅샷, refresh, with read only | 
```sql
CREATE OR REPLACE VIEW v_order_sum AS
SELECT o.cust_id, SUM(o.amt) amt FROM orders o GROUP BY o.cust_id
WITH READ ONLY;
CREATE MATERIALIZED VIEW mv_order_sum BUILD IMMEDIATE REFRESH COMPLETE ON DEMAND AS
SELECT o.cust_id, SUM(o.amt) amt FROM orders o GROUP BY o.cust_id;
BEGIN DBMS_MVIEW.REFRESH('MV_ORDER_SUM', 'C'); END;
```
- 뷰 안에 ORDER BY 를 넣지 않는다 (쓰는 쪽에서 정렬). 뷰를 거쳐 고칠 때 조건을 지키게 하려면 `WITH CHECK OPTION`.
- 구체화 뷰는 결과를 저장한다 — 빠른 새로 고침(FAST)은 원본 테이블에 MATERIALIZED VIEW LOG 가 있어야 한다.

@@ index | 인덱스 만들기 | 인덱스 생성, 인덱스 만들, 인덱스 추가, 인덱스 삭제, create index, drop index, 함수 기반 인덱스, unique index, 복합 인덱스, 결합 인덱스, invisible | 
```sql
CREATE INDEX ix_orders_cust_dt ON orders (cust_id, order_dt);        -- 같다(=) 조건 컬럼을 앞에, 범위 조건은 뒤에
CREATE UNIQUE INDEX ux_cust_email ON customers (email);
CREATE INDEX ix_cust_email_up ON customers (UPPER(email));           -- WHERE UPPER(email) = … 일 때 쓰인다
CREATE INDEX ix_orders_st ON orders (status) INVISIBLE;              -- 만들고 옵티마이저는 아직 안 씀 (11g+)
DROP INDEX ix_orders_st;
```
- 모든 키 컬럼이 NULL 인 행은 B-tree 인덱스에 없다 — `col IS NULL` 검색에는 안 쓰인다.
- 운영 중 큰 테이블은 `ONLINE` (Enterprise Edition). 인덱스가 많으면 DML 이 느려진다.

@@ exception | 예외 처리·오류 로그 | 예외, exception, 오류 처리, 에러 처리, when others, sqlerrm, sqlcode, 오류 로그, 에러 로그, backtrace, autonomous, pragma exception_init, raise | 
```sql
EXCEPTION
  WHEN DUP_VAL_ON_INDEX THEN …                  -- 이름 있는 예외: NO_DATA_FOUND, TOO_MANY_ROWS, ZERO_DIVIDE, VALUE_ERROR, INVALID_NUMBER
  WHEN OTHERS THEN
    v_code := SQLCODE; v_msg := SQLERRM;        -- SQL 문 안에 SQLERRM 을 바로 못 쓴다 → 변수에 담는다
    log_err(v_code, v_msg, DBMS_UTILITY.FORMAT_ERROR_BACKTRACE);   -- 몇 번째 줄인지
    RAISE;                                      -- 삼키지 말고 다시 던진다
```
- 롤백돼도 남는 오류 로그: `PROCEDURE log_err(…) IS PRAGMA AUTONOMOUS_TRANSACTION; BEGIN INSERT INTO err_log …; COMMIT; END;`
- ORA 번호에 이름 붙이기: `e_busy EXCEPTION; PRAGMA EXCEPTION_INIT(e_busy, -54);`
- `WHEN OTHERS THEN NULL;` 은 오류를 숨긴다 — 쓰지 않는다.

@@ lob | CLOB·BLOB (긴 텍스트) | clob, blob, nclob, lob, dbms_lob, 긴 텍스트, 긴 문자열, ora-22835, ora-22992, ora-01489 | 
- SQL 안의 VARCHAR2 는 4000바이트까지(EXTENDED 설정이면 32767). 그보다 긴 것은 CLOB.
- 일부만: `DBMS_LOB.SUBSTR(c, 1000, 1)` (글자 수, 시작). 길이 `DBMS_LOB.GETLENGTH(c)`. 찾기 `DBMS_LOB.INSTR(c, 'kw') > 0` (LIKE 도 된다).
- CLOB 는 `=`·GROUP BY·DISTINCT·ORDER BY·UNION 에 못 쓴다 (ORA-00932) → `DBMS_LOB.SUBSTR` 로 잘라 비교하거나 `DBMS_LOB.COMPARE(a, b) = 0`.
- 4000바이트 넘게 이으려면 앞을 CLOB 로: `TO_CLOB(a) || b`. CLOB → 문자 변환이 넘치면 ORA-22835.
- DB 링크 너머의 LOB 를 바로 SELECT 하면 ORA-22992 (옛 버전) → `INSERT INTO 로컬 SELECT … FROM t@link` 로 가져온다.

@@ timestamp | TIMESTAMP·INTERVAL·시간대 | timestamp, 타임스탬프, systimestamp, interval, 밀리초, 마이크로초, 시간대, 타임존, utc, extract, numtodsinterval, 초 단위, 초로 | 
```sql
TO_TIMESTAMP('2026-10-01 12:00:00.123', 'YYYY-MM-DD HH24:MI:SS.FF3')
TO_CHAR(SYSTIMESTAMP, 'YYYY-MM-DD HH24:MI:SS.FF3')
EXTRACT(HOUR FROM ts)            -- DATE 에는 YEAR/MONTH/DAY 만 (시·분은 TO_CHAR)
ts + NUMTODSINTERVAL(30, 'MINUTE')
(CAST(ts2 AS DATE) - CAST(ts1 AS DATE)) * 86400                  -- 초 차이 (초 미만 버림)
SYS_EXTRACT_UTC(SYSTIMESTAMP)
FROM_TZ(CAST(dt AS TIMESTAMP), 'Asia/Seoul') AT TIME ZONE 'UTC'
```
TIMESTAMP 끼리 빼면 숫자가 아니라 INTERVAL DAY TO SECOND 가 나온다.

@@ json | JSON (12c 이상) | json, json_value, json_query, json_table, json_object, json_arrayagg, is json | 12-
```sql
SELECT JSON_VALUE(t.doc, '$.customer.name') FROM orders_json t;          -- 값 하나
SELECT j.* FROM orders_json t,
       JSON_TABLE(t.doc, '$.items[*]' COLUMNS (item_id NUMBER PATH '$.id', qty NUMBER PATH '$.qty')) j;   -- 배열 → 행
SELECT JSON_OBJECT('id' VALUE o.order_id, 'amt' VALUE o.amt) FROM orders o;   -- 만들기 (12.2+)
SELECT JSON_ARRAYAGG(o.order_id ORDER BY o.order_id) FROM orders o;           -- (12.2+)
```
- 컬럼은 CLOB/VARCHAR2 에 `CHECK (doc IS JSON)`. 경로는 `$` 부터, 대소문자를 구분한다.

@@ temp | 임시·복사 테이블 (GTT·CTAS) | 임시 테이블, 임시테이블, temp table, temporary, global temporary, 백업 테이블, 테이블 복사, 복사본, create table as, ctas, select into 새 테이블 | 
- SQL Server 의 `#temp`, `SELECT … INTO 새테이블` 은 없다.
- 복사·백업: `CREATE TABLE orders_bak_20261021 AS SELECT * FROM orders WHERE …;` (제약·인덱스·기본값은 따라오지 않는다 — NOT NULL 만 온다). 구조만: `WHERE 1 = 0`.
- 세션별 임시 데이터: 테이블은 한 번만 만들어 두고 계속 쓴다 (프로시저마다 만들지 않는다)
```sql
CREATE GLOBAL TEMPORARY TABLE tmp_work (id NUMBER, amt NUMBER) ON COMMIT PRESERVE ROWS;   -- 세션 끝까지 (DELETE ROWS: 커밋하면 비움)
```
- 18c+ 는 `CREATE PRIVATE TEMPORARY TABLE ora$ptt_work …` (이름이 ORA$PTT_ 로 시작).

@@ korean | 한글 (정렬·길이·문자셋) | 한글, 가나다, 바이트 수, 글자 수, 문자셋, characterset, nls_sort, 전각, 반각, 깨짐, 깨져, 한글 정렬 | 
- 문자셋: `SELECT value FROM nls_database_parameters WHERE parameter = 'NLS_CHARACTERSET'` — AL32UTF8 은 한글 3바이트, KO16MSWIN949 는 2바이트.
- `VARCHAR2(10)` 은 기본이 바이트라 AL32UTF8 에서 한글 3자. 글자 수로 정하려면 `VARCHAR2(10 CHAR)`. 길이 `LENGTH`(글자) / `LENGTHB`(바이트).
- 바이트로 자르는 `SUBSTRB` 는 한글 중간에서 잘릴 수 있다 — 글자 단위는 `SUBSTR`.
- 언어 규칙으로 정렬: `ORDER BY NLSSORT(nm, 'NLS_SORT=KOREAN_M')`. 전각↔반각: `TO_SINGLE_BYTE` / `TO_MULTI_BYTE`.

@@ dblink | DB 링크 (다른 DB 조회) | db link, dblink, 디비링크, 디비 링크, 다른 db, 원격 db, 원격 테이블, database link, ora-02019, ora-02063, ora-22992 | 
```sql
SELECT * FROM orders@prod_link WHERE order_dt >= TRUNC(SYSDATE);
CREATE SYNONYM r_orders FOR app.orders@prod_link;                  -- 짧게 부르기
INSERT INTO orders_copy SELECT * FROM orders@prod_link WHERE …;   -- 가져오기
```
- 링크 이름이 틀리면 ORA-02019, 원격 쪽 오류는 ORA-02063 아래에 원래 오류가 붙는다.
- 원격 테이블과 큰 조인은 느리다 (행을 끌어온다) — 원격에서 먼저 거르거나 `/*+ DRIVING_SITE(r) */`.
- 원격 DML 도 내 COMMIT 으로 같이 확정된다. 원격 DDL 은 바로 못 한다.

@@ flashback | 실수 되돌리기 (FLASHBACK) | 실수로, 잘못 지운, 잘못 수정, 되살리, 복구, 되돌리, 원래대로, 이전 데이터, flashback, as of timestamp, 휴지통, recyclebin, before drop | 
커밋 전이면 `ROLLBACK`. 커밋한 뒤라면 (UNDO 가 남아 있는 동안 — 보통 몇 십 분~몇 시간):
```sql
SELECT * FROM orders AS OF TIMESTAMP (SYSTIMESTAMP - INTERVAL '10' MINUTE) WHERE order_id = 1001;   -- 10분 전 모습
INSERT INTO orders
SELECT * FROM orders AS OF TIMESTAMP (SYSTIMESTAMP - INTERVAL '10' MINUTE) o
 WHERE NOT EXISTS (SELECT 1 FROM orders x WHERE x.order_id = o.order_id);   -- 지운 행 되살리기
FLASHBACK TABLE orders TO BEFORE DROP;          -- DROP 한 테이블 (휴지통)
```
- 테이블 통째로 시점 되돌리기: `ALTER TABLE orders ENABLE ROW MOVEMENT;` 후 `FLASHBACK TABLE orders TO TIMESTAMP …;`
- TRUNCATE·DDL 뒤의 시점은 조회할 수 없다 (ORA-01466). 오래 지나 UNDO 가 덮이면 ORA-01555.

@@ hints | 힌트·실행계획 보기·통계 | 힌트, hint, /*+, parallel, append, use_nl, use_hash, leading, dbms_xplan, explain plan, 실제 실행계획, 통계 수집, 통계 정보, gather_table_stats, ora-12838 | 
```sql
SELECT /*+ INDEX(o ix_orders_dt) */ … FROM orders o …    -- 별칭을 붙였으면 힌트에도 별칭
/*+ FULL(o) */  /*+ LEADING(c o) USE_NL(o) */  /*+ USE_HASH(o) */  /*+ PARALLEL(o 4) */
INSERT /*+ APPEND */ INTO t SELECT …;                     -- 직접 경로. 커밋 전까지 그 테이블 조회 불가 (ORA-12838)
-- 실제로 돈 계획 (건수·시간 포함)
SELECT /*+ GATHER_PLAN_STATISTICS */ … ;
SELECT * FROM TABLE(DBMS_XPLAN.DISPLAY_CURSOR(NULL, NULL, 'ALLSTATS LAST'));
EXPLAIN PLAN FOR SELECT … ;  SELECT * FROM TABLE(DBMS_XPLAN.DISPLAY);   -- 예상 계획
BEGIN DBMS_STATS.GATHER_TABLE_STATS(USER, 'ORDERS'); END;              -- 통계 수집
```
힌트는 `/*+` 를 붙여 쓰고 SELECT·INSERT 등 바로 뒤에 둔다. 문법이 틀리면 오류 없이 무시된다.

@@ ora_bind | 바인드 변수 오류 (ORA-01008·01036·01006) | ora-01008, ora-01036, ora-01006, ora-01745, not all variables bound, illegal variable name, 바인드, bind, 바인드 변수, 치환 변수 | 
- ORA-01008: 바인드에 값을 다 주지 않았다. 동적 SQL 의 USING 은 이름이 아니라 위치 순서라, 같은 이름이 두 번 나오면 두 번 넘긴다.
- ORA-01036 / ORA-01006: SQL 의 바인드와 넘긴 이름·개수가 다르다. 주석·문자열 속 `:x` 는 바인드가 아니다. DDL 에는 바인드를 쓸 수 없다.
- `&변수` 는 SQL*Plus 치환 변수 — 일반 실행에서는 `:변수` 바인드를 쓴다.
- 바인드로 테이블·컬럼 이름은 바꿀 수 없다 (동적 SQL). `IN (:list)` 하나에 '1,2,3' 을 넣을 수 없다 → 쉼표 분리(REGEXP_SUBSTR … CONNECT BY LEVEL)로 행을 만들어 IN.

@@ ora_object | 객체·권한 오류 (ORA-00955·01031·04068·06508·01652) | ora-00955, ora-01031, ora-04063, ora-04068, ora-06508, ora-01652, pls-00905, insufficient privileges, name is already used, 권한, 권한 부여, grant, 동의어, synonym, 컴파일 오류, invalid 상태 | 
- ORA-00955: 이미 있는 이름 — 뷰·프로시저는 `CREATE OR REPLACE`, 테이블은 먼저 사전에서 확인한다.
- ORA-01031: 권한 부족 — `GRANT SELECT, INSERT ON app.orders TO scott;` 짧은 이름은 `CREATE SYNONYM orders FOR app.orders;`
  역할(ROLE)로 받은 권한은 정의자 권한 프로시저 안에서 통하지 않는다 → 직접 GRANT.
- ORA-04063 / ORA-06508 / PLS-00905: 객체가 INVALID — `ALTER PACKAGE pkg COMPILE BODY;` 후 `SELECT line, text FROM user_errors WHERE name = 'PKG'`.
- ORA-04068: 패키지를 다시 컴파일해서 세션의 패키지 상태가 버려졌다 — 한 번 더 실행하면 된다.
- ORA-01652: TEMP 공간 부족 — 큰 정렬·해시 조인. 조건으로 줄이거나 DBA 에게.
