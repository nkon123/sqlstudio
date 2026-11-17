# 데이터 검증 작업 폴더

이 폴더에서는 SQLStudio 가 분석한 PL/SQL·Pro*C·XML 코드의 **흐름을 따라가며 데이터가 코드대로 만들어졌는지 검증**한다.
코드를 고치는 곳이 아니다. 결과물은 `checks/` 아래의 검증 SQL 과 보고서뿐이다.

## 쓰는 도구 (MCP 서버 `sqlstudio`)

| 도구 | 무엇 | DB 에 붙나 |
|---|---|---|
| `list_connections` | 접속 목록. `run_check: true` 인 접속에서만 검증 SQL 을 돌릴 수 있다 | 아니오 |
| `flow_reports` / `flow_report` | 사람이 SQLStudio 에서 손본 흐름도 (상자·선·메모). **검증 범위와 업무 의도의 1차 근거** | 아니오 |
| `data_lineage` | 테이블에서 위(어디서 오나)·아래(어디로 가나)로 테이블 → 처리 → 테이블 단계 | 아니오 |
| `subprogram_sql` | 서브프로그램·패키지의 SQL 문을 소스 순서대로 (줄, 읽기·쓰기, INTO, 데이터를 대는 커서) | 아니오 |
| `column_writers` | 열 하나에 값을 넣는 문장과, 그 행을 대는 커서 쿼리 | 아니오 |
| `analysis_unit` / `subprogram_relations` / `table_usage` / `analysis_findings` | 패키지 요약·업무 규칙, 호출 관계, 테이블 CRUD, 확인할 것 | 아니오 |
| `describe_table` / `get_ddl` | 열·키·인덱스, 원본 소스 (잘린 SQL 을 볼 때) | 예 (사전 조회만) |
| `run_check` | 검증 SELECT 한 문장을 돌려 위반 행 수와 표본을 받는다 — 운영 접속은 늘 거절 | 예 |

## 지킬 것

- 데이터를 바꾸는 SQL 은 쓰지도 돌리지도 않는다. `run_check` 는 SELECT 한 문장만 받는다.
- 운영(production) 접속에는 검증 SQL 을 돌리지 않는다. 운영만 있으면 SQL 파일만 남기고 사람에게 넘긴다.
- 분석에 없는 테이블·열을 지어내지 않는다. 열 이름은 `describe_table` 로 확인한다.
- 근거를 늘 적는다: 서브프로그램 이름과 줄 번호, 흐름도의 선 설명·메모.
- `confidence: llm`, `source: llm`, 요약(summary)에서 나온 규칙은 **추정**으로 표시한다 — 정적 분석(커서 흐름, CRUD, SQL 본문)이 더 믿을 만하다.
- 서버 버전을 지킨다 (11g 면 FETCH FIRST·LATERAL·JSON 함수 없음 → ROWNUM).
