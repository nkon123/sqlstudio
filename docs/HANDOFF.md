# 인계 메모 (클라우드 세션 → 로컬)

마지막 커밋 기준 상태와, 로컬에서 이어 갈 때 필요한 것.

## 로컬 준비

```bash
git clone https://github.com/nkon123/sqlstudio && cd sqlstudio
# Rust 1.80+ (rust-toolchain stable), Node 20+
cd app && npm ci && cd ..
cargo test --workspace                       # DB 없이 도는 테스트 전부
cd app && npx tsc --noEmit && npx tauri dev  # 앱 (Windows 는 WebView2, Linux 는 webkit2gtk-4.1)
```

### Oracle 11g 통합 테스트

```bash
docker run -d --name ora11 -p 1521:1521 -e ORACLE_PASSWORD=oracle gvenzl/oracle-xe:11-slim
# Instant Client 19 (basic lite) 를 받아 LD_LIBRARY_PATH / PATH 에
SQLS_TEST_DSN=system/oracle@localhost:1521/XE cargo test --workspace -- --ignored --test-threads=1
```

`localhost` 포트 매핑이면 Docker 가 취소 신호(OOB)를 버린다 — `abandon_releases_stuck_call` 은 그래도 통과해야 한다.

## 이번 세션에서 한 것 (커밋 순)

| 커밋 | 내용 |
|---|---|
| PL/SQL 디버거 화면 | DBMS_DEBUG, F5/F10/F11, 중단점·변수·감시·스택 |
| PL/SQL 분석 | 조각 → 조각별 JSON → 통합, CLI `sqlstudio-analyze`, 앱 '분석' |
| SQL 문 단위·커서 → DML 흐름 | `flows.json`, `statements.json` |
| 긴 SQL 을 절 경계에서 자르기 | CTE → 절 → 쉼표·AND, SQL 하나로 요약 |
| 커서 뜻 먼저 | 커서를 쓰는 조각에 뜻을 넣는다 |
| 품질 평가 `eval` | 근거 없는 이름, 조각 크기 권장, 모델 비교 |
| 세션·락 모니터 | 툴바 '세션', KILL 은 별도 세션으로 |
| MCP 분석 툴 5개 | DB 에 붙지 않고 분석 JSON 만 |
| 비밀번호 OS 저장 | Windows 자격 증명 관리자, MCP·CLI 도 읽음 |
| 결과 그리드 정렬·필터·편집 | 이 인계 직전 커밋 |

## 확인이 덜 된 것 (로컬에서 먼저 볼 것)

1. **실제 로컬 모델로 분석** — 클라우드에서는 모델을 받을 수 없어 가짜 서버로만 시험했다.
   `sqlstudio-analyze run --connection <접속> --name "XX%" --limit 20 --llm "<공급자>" --out D:\eval\m1` →
   `sqlstudio-analyze eval --out D:\eval\m1` 의 `integrated/eval.md` 를 보고 프롬프트(`crates/sqls-analyze/src/llm.rs`)·
   조각 크기(`chunk.rs` `Limits`)를 조정한다.
2. **Windows 자격 증명 관리자** — CI windows 잡의 `cargo test -p sqls-core --lib secret` 가 저장·읽기를 본다.
   실제 앱에서 '비밀번호 저장' → 앱 재시작 → 묻지 않고 붙는지 확인.
3. **그리드 편집 화면** — 셀 고치기(더블클릭/F2)·적용 확인창은 확인했다. **'행 삭제' 버튼과 Delete 키로 지울 행 표시는
   화면에서 확인하지 못했다** (Delete 키가 표시를 안 하는 것처럼 보였다 — `app/src/grid.ts` `onKey` / `toggleDeleteSelected`).
   서버 쪽 DELETE 는 `crates/sqls-core/tests/edit_live.rs` 가 11g 에서 확인한다.
4. 사내망 Windows 설치 (Instant Client 경로, CP949, 프록시).

## 다음 할 일 (추천 순서)

- **B. ALL_DEPENDENCIES 교차 검증** — 통합 분석에서 못 푼 호출(동의어·동적 SQL)·테이블을 DB 의존성으로 보완.
  `crates/sqls-analyze/src/integrate.rs` `resolve()` 뒤에, `source.rs` 에서 `ALL_DEPENDENCIES` 를 읽어 넘긴다.
- **C. 영향 분석 화면 + 컬럼 단위 계보** — 테이블·컬럼 → 읽고 쓰는 곳·흐름·시작점 트리. `flow.rs` 의 DML 에서
  INSERT 열 목록·UPDATE SET 열과 커서 SELECT 목록을 짝지으면 컬럼 계보가 된다.
- **D. SQL 튜닝 보조** — `DBMS_XPLAN.DISPLAY_CURSOR`(실제 계획), `V$SQL` 상위 SQL, `V$SQL_BIND_CAPTURE`.
  `crates/sqls-core/src/monitor.rs` 옆에.
- **E. 문서 생성 + YAML** — 분석 결과로 패키지 설명서(HTML/MD), JSON 을 YAML 로 내보내기.
- **F. 스키마 비교·DDL 내보내기**.
- **G. 대규모 성능** — 테이블 수만·소스 수백만 줄로 자동완성·분석·통합 시간 재기.

## 지켜야 할 원칙

- **AI(LLM) 경로로 SQL 을 실행하지 않는다.** AI 답은 에디터 텍스트로만, MCP 툴은 사전 조회·분석 JSON 읽기만.
  `crates/sqls-mcp/src/tests.rs` 의 `no_tool_executes_sql` 이 툴 목록을 고정한다 — 툴을 더하면 거기도 고친다.
- 사람이 실행하는 것(그리드 편집, 세션 종료, 디버그)은 문장을 보여 주고 확인을 받는다. 읽기 전용 프로필에서는 막는다.
- 비밀번호·API 키는 설정 파일에 쓰지 않는다.
