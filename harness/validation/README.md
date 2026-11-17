# 데이터 검증 하네스 (OpenCode + SQLStudio 흐름도)

SQLStudio 의 분석 결과(흐름도·SQL 문·커서 흐름)를 OpenCode 에 MCP 로 주고,
OpenCode 가 **코드 흐름을 따라가며 검증 규칙을 뽑아 SELECT 로 확인**하게 하는 작업 폴더다.

```
흐름도 보고서 / 테이블 / 열
   │ flow_report · data_lineage · column_writers      (분석 결과 JSON — DB 안 씀)
   ▼
처리마다 SQL 문을 소스 순서대로 따라가기
   │ subprogram_sql · subprogram_relations · get_ddl
   ▼
검증 규칙 (건수 대사 · 값 재계산 · 키 무결성 · 필수값 · 상태 전이 · 업무 규칙)
   │ describe_table 로 열·키 확인
   ▼
위반 행을 돌려주는 SELECT → run_check (운영이 아닌 접속, 읽기 전용, 호출마다 기록)
   ▼
checks/<대상>/checks.sql · report.md
```

## 준비

1. **분석 돌리기** — SQLStudio 의 분석 화면, 또는 `sqlstudio-analyze run --connection <접속>`.
   업무 흐름이 정해져 있으면 **분석 > 흐름도**에서 보고서로 저장하고 메모·선 설명을 달아 둔다. 에이전트가 이것을 검증 기준으로 쓴다.
2. **MCP 설정** (`config.toml`, 설정 파일 옆):
   ```toml
   [mcp]
   allowed_connections = ["ERP-DEV"]   # 검증할 접속 (운영이 아닌 것)
   allow_checks = true                 # run_check 툴을 켠다
   check_max_rows = 20                 # 위반 행 표본 수
   call_timeout_secs = 60
   ```
   운영(`production = true`) 프로필은 `allow_checks` 를 켜도 `run_check` 가 늘 거절한다.
   비밀번호는 앱에서 저장했거나 `SQLSTUDIO_PW_<접속>` 환경변수.
3. **이 폴더를 검증 작업 폴더로 복사**하고 `opencode.json` 의 `command` 경로를 맞춘다
   (`sqlstudio-mcp.exe` 위치, `config.toml` 위치 — 포터블이면 `data\config.toml`).
4. 확인:
   ```bash
   opencode mcp list
   ```
   ```bash
   opencode agent list
   ```
   `sqlstudio connected` 와 `validator (primary)` 가 보이면 된다.

## 쓰기

```bash
opencode
```
TUI 에서 `/validate 주문 마감 흐름` · `/validate APP.ORDER_LOG` · `/validate APP.ORDER_LOG.AMT 기준일 2026-11-09`.

한 번에 돌리고 끝내려면:
```bash
opencode run --agent validator --command validate "APP.ORDER_LOG 기준일 2026-11-09"
```

## 파일

| 파일 | 무엇 |
|---|---|
| `opencode.json` | MCP 서버 `sqlstudio`, 기본 에이전트 `validator`, 권한 (bash·webfetch 금지, `run_check` 은 묻기) |
| `AGENTS.md` | 이 폴더의 규칙과 도구 표 — 모든 에이전트가 읽는다 |
| `.opencode/agent/validator.md` | 검증 절차: 범위 → 흐름 따라가기 → 규칙 → SQL → 실행 → 보고서 |
| `.opencode/command/validate.md` | `/validate <대상>` |

## 안전

- `run_check` 는 `allow_checks = true` 일 때만 생긴다. SELECT/WITH **한 문장**만 받고 (FOR UPDATE·DML·DDL·PL/SQL·여러 문장은 DB 에 보내기 전에 거절),
  세션은 `SET TRANSACTION READ ONLY`, 쿼리마다 시간 상한이 있다.
- 호출마다 (거절 포함) 설정 파일 옆 `mcp-checks.jsonl` 에 시각·접속·목적·SQL·결과 수가 남는다.
- 읽기 전용 트랜잭션도 **자율 트랜잭션 함수**(PRAGMA AUTONOMOUS_TRANSACTION) 안의 쓰기는 막지 못한다. 그래서 운영은 늘 빼고,
  검증 접속은 가능하면 SELECT 권한만 있는 계정으로 만든다.
- `opencode.json` 의 `"sqlstudio_run_check": "ask"` 이면 실행마다 사람이 승인한다. 무인으로 돌릴 때만 `"allow"` 로 바꾼다.
- 위반 행 표본은 모델 공급자에게 간다. 개인정보가 있는 테이블이면 로컬 모델을 쓰거나 `check_max_rows` 를 줄이고,
  SELECT 에 키만 넣게 한다.
