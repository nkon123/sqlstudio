//! SQLStudio 코어.
//!
//! - [`sql`]     : SQL 텍스트 분석 (DB 없이 동작)
//! - [`session`] : Oracle 세션 — 접속마다 전용 작업 스레드 하나
//! - [`meta`]    : 데이터 사전 조회 (테이블 구조, DDL, 실행계획)
//! - [`config`]  : 접속 프로필
//!
//! UI 스레드와 async 런타임은 절대 OCI 호출을 직접 하지 않는다. 모든 DB 작업은
//! 세션 스레드에서 돌고, 결과는 채널로 돌아온다. 그래서 느린 쿼리가 화면을
//! 멈추지 않고, 한 세션의 문제가 다른 세션으로 번지지 않는다.

pub mod config;
pub mod error;
pub mod meta;
pub mod session;
pub mod sql;

pub use error::{Error, ErrorInfo, Result};
pub use session::{Column, ConnectSpec, ExecOptions, ExecOutcome, ExecResult, RowPage, Session};
pub use sql::{Statement, StmtKind};
