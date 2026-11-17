---
description: 흐름도·테이블·열 하나를 골라 데이터 검증 (예 — /validate 주문 마감 흐름 · /validate APP.ORDER_LOG · /validate APP.ORDER_LOG.AMT 기준일 2026-11-09)
agent: validator
---

검증 대상: $ARGUMENTS

validator 절차대로 진행한다. 대상이 비어 있으면 flow_reports 로 목록을 보여 주고 고르게 한다.
기준(날짜·배치 번호)이 없고 테이블이 크면 먼저 물어본다.
