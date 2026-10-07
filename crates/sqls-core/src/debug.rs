//! PL/SQL 디버거 — DBMS_DEBUG (11g 에서 방화벽 없이 되는 방식. Toad 와 같다).
//!
//! 세션 두 개를 쓴다.
//! - **대상 세션**: 디버그할 블록을 실행한다. 멈춰 있는 동안 그 호출은 돌아오지 않는다
//!   (그래서 전용 스레드 세션 구조가 그대로 맞는다).
//! - **제어 세션**: 대상에 붙어서 중단점·한 줄 실행·변수·호출 스택을 다룬다.
//!
//! DBMS_DEBUG 는 PL/SQL 레코드(runtime_info, program_info)를 주고받는데, OCI 로는 레코드를
//! 바인드할 수 없다. 그래서 모든 호출을 익명 블록으로 감싸 스칼라 OUT 바인드로 꺼낸다.
//! 상수(break_*, reason_*, namespace_*)는 서버에서 읽는다 — 버전마다 값이 달라도 맞게.
//!
//! 필요한 권한: DEBUG CONNECT SESSION, 그리고 대상 단위에 대한 DEBUG (남의 것은 DEBUG ANY PROCEDURE).
//! 대상 단위는 디버그 정보로 컴파일되어 있어야 한다 (`ALTER ... COMPILE DEBUG`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Notify;

use crate::error::{Error, Result};
use crate::session::{ConnectSpec, ExecOptions, ExecResult, Session};

/// 제어 세션이 한 번에 기다리는 시간(초). 넘으면 대상이 끝났는지 보고 다시 기다린다.
const WAIT_SLICE_SECS: i64 = 3;
/// 대상이 디버거 응답을 기다리는 상한(초). 넘으면 디버그를 끄고 끝까지 실행한다.
/// 한 줄에서 이만큼 생각하면 그 세션은 풀린다 — 화면에 알린다.
pub const TARGET_IDLE_SECS: i64 = 900;

/// 멈춘 곳
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stop {
    /// 대상이 끝났는지 (정상 종료·오류·중지)
    pub terminated: bool,
    pub line: u32,
    /// 단위 소유자 / 이름 (익명 블록이면 빈 문자열)
    pub owner: String,
    pub name: String,
    /// ALL_SOURCE 의 TYPE (PROCEDURE, PACKAGE BODY ...) — 익명 블록이면 "ANONYMOUS BLOCK"
    pub unit_type: String,
    /// breakpoint | step | exception | start | finished | abort | other
    pub reason: String,
    pub depth: u32,
    /// 멈춘 중단점 번호 (중단점에서 멈췄을 때)
    pub breakpoint: Option<i64>,
    /// 예외에서 멈췄으면 ORA 번호
    pub ora_code: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// 호출 안으로 들어간다
    Into,
    /// 다음 줄 (호출은 넘긴다)
    Over,
    /// 지금 서브프로그램을 끝내고 부른 곳으로
    Out,
    /// 다음 중단점(또는 끝)까지
    Run,
    /// 실행을 멈춘다 (대상의 호출은 오류로 끝난다)
    Abort,
}

#[derive(Debug, Clone, Serialize)]
pub struct Frame {
    pub depth: u32,
    pub owner: String,
    pub name: String,
    pub unit_type: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct VarValue {
    pub name: String,
    /// 값 (NULL 은 None). 읽을 수 없으면 `error` 에 이유
    pub value: Option<String>,
    pub error: Option<String>,
}

/// 서버에서 읽은 DBMS_DEBUG 상수
#[derive(Debug, Clone)]
struct Consts {
    b_next: i64,
    b_call: i64,
    b_ret: i64,
    b_exc: i64,
    abort: i64,
    info: i64,
    success: i64,
    e_timeout: i64,
    reasons: HashMap<i64, &'static str>,
    exit_reasons: Vec<i64>,
    ns_body: i64,
    ns_top: i64,
    ns_trigger: i64,
    lu: HashMap<i64, &'static str>,
    errors: HashMap<i64, &'static str>,
}

const CONSTS_SQL: &str = "BEGIN
 :b_next := dbms_debug.break_next_line; :b_call := dbms_debug.break_any_call;
 :b_ret := dbms_debug.break_any_return; :b_exc := dbms_debug.break_exception;
 :abort := dbms_debug.abort_execution;
 :info := dbms_debug.info_getstackdepth + dbms_debug.info_getbreakpoint
        + dbms_debug.info_getlineinfo + dbms_debug.info_getoerinfo;
 :success := dbms_debug.success; :e_timeout := dbms_debug.error_timeout;
 :e_nosuch := dbms_debug.error_no_such_object; :e_unknown := dbms_debug.error_unknown_type;
 :e_nodebug := dbms_debug.error_no_debug_info; :e_badframe := dbms_debug.error_bogus_frame;
 :e_illegal := dbms_debug.error_illegal_line; :e_badhandle := dbms_debug.error_bad_handle;
 :e_indexed := dbms_debug.error_indexed_table; :e_nullcol := dbms_debug.error_nullcollection;
 :r_bp := dbms_debug.reason_breakpoint; :r_enter := dbms_debug.reason_enter;
 :r_return := dbms_debug.reason_return; :r_finish := dbms_debug.reason_finish;
 :r_line := dbms_debug.reason_line; :r_exc := dbms_debug.reason_exception;
 :r_exit := dbms_debug.reason_exit; :r_knl := dbms_debug.reason_knl_exit;
 :r_abort := dbms_debug.reason_abort; :r_start := dbms_debug.reason_interpreter_starting;
 :r_handler := dbms_debug.reason_handler;
 :ns_body := dbms_debug.namespace_pkg_body; :ns_top := dbms_debug.namespace_pkgspec_or_toplevel;
 :ns_trigger := dbms_debug.namespace_trigger;
 :lu_proc := dbms_debug.libunittype_procedure; :lu_func := dbms_debug.libunittype_function;
 :lu_pkg := dbms_debug.libunittype_package; :lu_body := dbms_debug.libunittype_package_body;
 :lu_trig := dbms_debug.libunittype_trigger; :lu_cursor := dbms_debug.libunittype_cursor;
END;";

const CONSTS_OUT: &[&str] = &[
    "b_next", "b_call", "b_ret", "b_exc", "abort", "info", "success", "e_timeout",
    "e_nosuch", "e_unknown", "e_nodebug", "e_badframe", "e_illegal", "e_badhandle", "e_indexed", "e_nullcol",
    "r_bp", "r_enter", "r_return", "r_finish", "r_line", "r_exc", "r_exit", "r_knl", "r_abort", "r_start", "r_handler",
    "ns_body", "ns_top", "ns_trigger",
    "lu_proc", "lu_func", "lu_pkg", "lu_body", "lu_trig", "lu_cursor",
];

/// runtime_info 를 스칼라로 꺼내는 공통 꼬리
const RI_OUT: &str = ":ret := ret; :term := ri.terminated; :line := ri.line#; :bp := ri.breakpoint;
 :reason := ri.reason; :depth := ri.stackdepth; :oer := ri.oer;
 :owner := ri.program.owner; :name := ri.program.name; :lu := ri.program.libunittype;";
const RI_NAMES: &[&str] = &["ret", "term", "line", "bp", "reason", "depth", "oer", "owner", "name", "lu"];

fn num(v: &Option<String>) -> i64 {
    v.as_deref().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// 대상 세션의 실행 결과 (끝날 때 채워진다)
type TargetResult = Arc<Mutex<Option<Result<ExecResult>>>>;

pub struct Debugger {
    target: Session,
    ctl: Session,
    c: Consts,
    result: TargetResult,
    done: Arc<Notify>,
    /// 실행 블록 원문 (익명 블록의 소스)
    pub block: String,
    finished: bool,
    /// finish() 를 거쳤는지 — 아니면 Drop 에서 세션을 끊는다
    closed: bool,
    /// (owner, name) → ALL_SOURCE 의 TYPE
    kinds: HashMap<(String, String), String>,
}

impl Drop for Debugger {
    /// 어떤 경로로든 디버거가 사라지면 세션을 끊는다. 대상 세션이 디버거를 기다리며
    /// 패키지 잠금을 쥐고 남으면 다른 세션의 컴파일이 멈춘다.
    fn drop(&mut self) {
        if !self.closed {
            self.target.abandon();
            self.ctl.abandon();
        }
    }
}

impl Debugger {
    /// 디버그 세션을 열고 블록 실행을 시작한다. 블록의 첫 줄에서 멈춘 상태로 돌아온다.
    pub async fn start(spec: ConnectSpec, block: &str, binds: Vec<(String, Option<String>)>) -> Result<(Debugger, Stop)> {
        let mut tspec = spec.clone();
        tspec.read_only = false;
        tspec.dbms_output = true;
        tspec.module = "SQLStudio-Debug".into();
        let mut cspec = spec;
        cspec.read_only = false;
        cspec.dbms_output = false;
        cspec.module = "SQLStudio-Debugger".into();
        cspec.call_timeout = None;
        let (target, ctl) = tokio::try_join!(Session::connect(tspec), Session::connect(cspec))?;

        let c = load_consts(&ctl).await?;

        // 대상: 익명 블록도 디버그 정보로 컴파일하고, 디버그를 켠다
        target.execute("ALTER SESSION SET PLSQL_DEBUG = TRUE", ExecOptions::default()).await?;
        let id = target
            .call_plsql(
                // 디버거(앱)가 죽어도 대상이 영원히 기다리지 않게: 응답이 없으면 디버그를 끄고 끝까지 실행한다.
                // (기다리는 동안 패키지 잠금을 쥐고 있어 다른 세션의 컴파일이 멈춘다)
                "DECLARE r BINARY_INTEGER; BEGIN
                   :id := dbms_debug.initialize(diagnostics => 0);
                   r := dbms_debug.set_timeout(:t);
                   dbms_debug.set_timeout_behaviour(dbms_debug.nodebug_on_timeout);
                   dbms_debug.debug_on(TRUE, FALSE); END;",
                vec![("t".into(), Some(TARGET_IDLE_SECS.to_string()))],
                &["id"],
            )
            .await
            .map_err(privilege_hint)?
            .remove(0)
            .ok_or_else(|| Error::Invalid("디버그 세션 ID 를 받지 못했습니다".into()))?;
        // 제어: 붙고, 기다리는 단위를 짧게 (대상이 끝났는지 틈틈이 보려고)
        ctl.call_plsql(
            "DECLARE r BINARY_INTEGER; BEGIN dbms_debug.attach_session(:id); r := dbms_debug.set_timeout(:t); END;",
            vec![("id".into(), Some(id)), ("t".into(), Some(WAIT_SLICE_SECS.to_string()))],
            &[],
        )
        .await
        .map_err(privilege_hint)?;
        // 사용자 블록을 감싸 같은 호출 안에서 디버그를 끈다. 끄지 않으면 이 호출 뒤의 PL/SQL 호출
        // (DBMS_OUTPUT 읽기 등)도 디버그 대상이 되어 디버거를 영원히 기다린다.
        // BEGIN 을 첫 줄에 붙여 줄 번호가 바뀌지 않게 한다.
        let wrapped = wrap_block(block);

        // 대상 실행 — 돌아오지 않는다 (디버거가 놓아줄 때까지)
        let result: TargetResult = Arc::new(Mutex::new(None));
        let done = Arc::new(Notify::new());
        {
            let (t, r, d, b) = (target.clone(), result.clone(), done.clone(), wrapped);
            tokio::spawn(async move {
                let out = t
                    .execute(&b, ExecOptions { binds, first_page: 200, ..Default::default() })
                    .await;
                *r.lock().unwrap() = Some(out);
                d.notify_waiters();
            });
        }

        let mut dbg = Debugger {
            target,
            ctl,
            c,
            result,
            done,
            block: block.to_string(),
            finished: false,
            closed: false,
            kinds: HashMap::new(),
        };
        let mut first = dbg.wait_event(true, 0).await?;
        // 첫 사건은 "인터프리터 시작"(줄 0)이다. 여기서 "다음 줄"을 하면 끝까지 달려 버리므로
        // 한 번 들어가서 블록의 첫 줄에 세운다.
        if !first.terminated && first.line == 0 {
            first = dbg.wait_event(false, dbg.c.b_call).await?;
        }
        Ok((dbg, first))
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn target_done(&self) -> bool {
        self.result.lock().unwrap().is_some()
    }

    /// 다음 사건(멈춤·끝)까지 기다린다. `sync` 면 SYNCHRONIZE(첫 대기), 아니면 CONTINUE.
    async fn wait_event(&mut self, sync: bool, flags: i64) -> Result<Stop> {
        let info = self.c.info.to_string();
        let mut first = !sync;
        loop {
            let (sql, ins) = if first {
                (
                    format!("DECLARE ri dbms_debug.runtime_info; ret BINARY_INTEGER; BEGIN ret := dbms_debug.continue(ri, :flags, :info); {RI_OUT} END;"),
                    vec![("flags".to_string(), Some(flags.to_string())), ("info".to_string(), Some(info.clone()))],
                )
            } else {
                (
                    format!("DECLARE ri dbms_debug.runtime_info; ret BINARY_INTEGER; BEGIN ret := dbms_debug.synchronize(ri, :info); {RI_OUT} END;"),
                    vec![("info".to_string(), Some(info.clone()))],
                )
            };
            first = false;
            let v = self.ctl.call_plsql(&sql, ins, RI_NAMES).await?;
            let ret = num(&v[0]);
            if ret == self.c.e_timeout {
                // 대상이 아직 돌고 있다 — 끝났으면 그만 기다린다
                if self.target_done() {
                    return Ok(self.finish_stop("finished"));
                }
                continue;
            }
            if ret != self.c.success {
                // 대상이 끝나 연결이 끊긴 경우 등
                if self.target_done() {
                    return Ok(self.finish_stop("finished"));
                }
                let msg = self.c.errors.get(&ret).copied().unwrap_or("알 수 없는 오류");
                return Err(Error::Invalid(format!("디버거 오류 {ret}: {msg}")));
            }
            let reason_code = num(&v[4]);
            let terminated = num(&v[1]) != 0 || self.c.exit_reasons.contains(&reason_code);
            if terminated {
                // 대상 호출이 돌아올 때까지 잠깐 기다린다 (결과·DBMS_OUTPUT)
                let done = self.done.clone();
                if !self.target_done() {
                    let _ = tokio::time::timeout(Duration::from_secs(10), done.notified()).await;
                }
                return Ok(self.finish_stop(if reason_code == self.c.abort { "abort" } else { "finished" }));
            }
            let reason = self.c.reasons.get(&reason_code).copied().unwrap_or("other");
            let name = v[8].clone().unwrap_or_default();
            let owner = v[7].clone().unwrap_or_default();
            let unit_type = if name.is_empty() {
                "ANONYMOUS BLOCK".to_string()
            } else {
                self.unit_type(&owner, &name, v[9].as_deref()).await
            };
            let bp = num(&v[3]);
            let oer = num(&v[6]);
            return Ok(Stop {
                terminated: false,
                line: num(&v[2]) as u32,
                owner,
                name,
                unit_type,
                reason: reason.to_string(),
                depth: num(&v[5]) as u32,
                breakpoint: (bp > 0).then_some(bp),
                ora_code: (oer != 0).then_some(oer.abs()),
            });
        }
    }

    /// 단위 종류. runtime_info 가 비워 둘 때가 있어(11g) 사전에서 찾는다 — 실행 코드는 본문에 있다.
    async fn unit_type(&mut self, owner: &str, name: &str, lu: Option<&str>) -> String {
        if let Some(t) = lu.and_then(|x| x.trim().parse::<i64>().ok()).and_then(|n| self.c.lu.get(&n)) {
            if *t != "CURSOR" {
                return t.to_string();
            }
        }
        let key = (owner.to_string(), name.to_string());
        if let Some(t) = self.kinds.get(&key) {
            return t.clone();
        }
        let t = self
            .ctl
            .query(
                "SELECT object_type FROM all_objects WHERE owner = :o AND object_name = :n \
                 AND object_type IN ('PACKAGE BODY', 'TYPE BODY', 'PROCEDURE', 'FUNCTION', 'TRIGGER') \
                 ORDER BY DECODE(object_type, 'PACKAGE BODY', 1, 'TYPE BODY', 2, 3)",
                vec![("O".into(), Some(owner.to_string())), ("N".into(), Some(name.to_string()))],
                1,
            )
            .await
            .ok()
            .and_then(|p| p.rows.first().and_then(|r| r[0].clone()))
            .unwrap_or_else(|| "UNKNOWN".into());
        self.kinds.insert(key, t.clone());
        t
    }

    fn finish_stop(&mut self, reason: &str) -> Stop {
        self.finished = true;
        Stop {
            terminated: true,
            line: 0,
            owner: String::new(),
            name: String::new(),
            unit_type: String::new(),
            reason: reason.to_string(),
            depth: 0,
            breakpoint: None,
            ora_code: None,
        }
    }

    /// 한 걸음. `break_on_exception` 이면 예외가 나는 곳에서도 멈춘다.
    pub async fn step(&mut self, step: Step, break_on_exception: bool) -> Result<Stop> {
        if self.finished {
            return Ok(self.finish_stop("finished"));
        }
        let mut flags = match step {
            Step::Into => self.c.b_call,
            Step::Over => self.c.b_next,
            Step::Out => self.c.b_ret,
            Step::Run => 0,
            Step::Abort => self.c.abort,
        };
        if break_on_exception && step != Step::Abort {
            flags += self.c.b_exc;
        }
        self.wait_event(false, flags).await
    }

    /// 대상이 돌고 있을 때 멈추게 한다 (긴 루프 등). 대상 호출은 ORA-01013 으로 끝난다.
    pub fn interrupt(&self) -> Result<()> {
        self.target.cancel()
    }

    /// 중단점. 단위 종류는 ALL_SOURCE 의 TYPE (PACKAGE BODY, PROCEDURE ...).
    pub async fn set_breakpoint(&self, owner: &str, name: &str, unit_type: &str, line: u32) -> Result<i64> {
        let ns = match unit_type {
            "PACKAGE BODY" | "TYPE BODY" => self.c.ns_body,
            "TRIGGER" => self.c.ns_trigger,
            _ => self.c.ns_top,
        };
        let v = self
            .ctl
            .call_plsql(
                "DECLARE pi dbms_debug.program_info; bp BINARY_INTEGER; ret BINARY_INTEGER; BEGIN
                   pi.namespace := :ns; pi.name := :name; pi.owner := :owner; pi.dblink := NULL; pi.entrypointname := NULL;
                   ret := dbms_debug.set_breakpoint(pi, :line, bp, 0, 0);
                   :ret := ret; :bp := bp; END;",
                vec![
                    ("ns".into(), Some(ns.to_string())),
                    ("name".into(), Some(name.to_uppercase())),
                    ("owner".into(), Some(owner.to_uppercase())),
                    ("line".into(), Some(line.to_string())),
                ],
                &["ret", "bp"],
            )
            .await?;
        let ret = num(&v[0]);
        if ret != self.c.success {
            let why = self.c.errors.get(&ret).copied().unwrap_or("알 수 없는 오류");
            return Err(Error::Invalid(format!("{owner}.{name} {line}행에 중단점을 둘 수 없습니다: {why}")));
        }
        Ok(num(&v[1]))
    }

    pub async fn delete_breakpoint(&self, bp: i64) -> Result<()> {
        self.ctl
            .call_plsql(
                "DECLARE r BINARY_INTEGER; BEGIN r := dbms_debug.delete_breakpoint(:bp); END;",
                vec![("bp".into(), Some(bp.to_string()))],
                &[],
            )
            .await?;
        Ok(())
    }

    /// 변수 값. `frame` 은 호출 스택의 깊이 그대로다: 0 = 지금 멈춘 곳,
    /// 그 밖에는 [`Frame::depth`] (1 = 가장 바깥 블록). 부른 쪽은 `stop.depth - 1`.
    pub async fn get_value(&self, name: &str, frame: u32) -> Result<VarValue> {
        let v = self
            .ctl
            .call_plsql(
                "DECLARE v VARCHAR2(32767); ret BINARY_INTEGER; BEGIN
                   ret := dbms_debug.get_value(:n, :f, v, NULL); :ret := ret; :l_v := v; END;",
                vec![("n".into(), Some(name.to_string())), ("f".into(), Some(frame.to_string()))],
                &["ret", "l_v"],
            )
            .await?;
        let ret = num(&v[0]);
        Ok(if ret == self.c.success {
            VarValue { name: name.to_string(), value: v[1].clone(), error: None }
        } else {
            let why = self.c.errors.get(&ret).copied().unwrap_or("읽을 수 없음");
            VarValue { name: name.to_string(), value: None, error: Some(why.to_string()) }
        })
    }

    /// 변수 바꾸기: `assignment` 는 `x := 10;` 형태. `frame` 은 [`Debugger::get_value`] 와 같다.
    pub async fn set_value(&self, frame: u32, assignment: &str) -> Result<()> {
        let v = self
            .ctl
            .call_plsql(
                "DECLARE ret BINARY_INTEGER; BEGIN ret := dbms_debug.set_value(:f, :a); :ret := ret; END;",
                vec![("f".into(), Some(frame.to_string())), ("a".into(), Some(assignment.to_string()))],
                &["ret"],
            )
            .await?;
        let ret = num(&v[0]);
        if ret != self.c.success {
            let why = self.c.errors.get(&ret).copied().unwrap_or("바꿀 수 없음");
            return Err(Error::Invalid(format!("{assignment} — {why}")));
        }
        Ok(())
    }

    /// 호출 스택 (안쪽 먼저)
    pub async fn backtrace(&mut self) -> Result<Vec<Frame>> {
        let v = self
            .ctl
            .call_plsql(
                "DECLARE bt dbms_debug.backtrace_table; s VARCHAR2(32767); i BINARY_INTEGER; BEGIN
                   dbms_debug.print_backtrace(bt);
                   i := bt.FIRST;
                   WHILE i IS NOT NULL LOOP
                     s := s || i || '|' || bt(i).owner || '|' || bt(i).name || '|' || bt(i).libunittype || '|' || bt(i).line# || CHR(10);
                     i := bt.NEXT(i);
                   END LOOP;
                   :l_s := s; END;",
                vec![],
                &["l_s"],
            )
            .await?;
        let mut frames: Vec<Frame> = v[0]
            .clone()
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let p: Vec<&str> = l.split('|').collect();
                if p.len() < 5 {
                    return None;
                }
                let name = p[2].to_string();
                Some((
                    Frame {
                        depth: p[0].parse().ok()?,
                        owner: p[1].to_string(),
                        unit_type: if name.is_empty() { "ANONYMOUS BLOCK".into() } else { String::new() },
                        name,
                        line: p[4].parse().unwrap_or(0),
                    },
                    p[3].to_string(),
                ))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        for f in frames.iter_mut() {
            if f.unit_type.is_empty() {
                let (o, n) = (f.owner.clone(), f.name.clone());
                f.unit_type = self.unit_type(&o, &n, None).await;
            }
        }
        // DBMS_DEBUG 는 바깥(1)부터 준다 — 화면은 안쪽부터
        frames.sort_by(|a, b| b.depth.cmp(&a.depth));
        Ok(frames)
    }

    /// 끝낸다. 대상이 아직 멈춰 있으면 중지시키고, 대상 세션의 변경은 `commit` 이 아니면 롤백한다.
    /// 대상 블록의 실행 결과(DBMS_OUTPUT 포함)를 돌려준다.
    pub async fn finish(mut self, commit: bool) -> Option<Result<ExecResult>> {
        self.closed = true;
        if !self.finished {
            let _ = self.wait_event(false, self.c.abort).await;
        }
        let _ = self.ctl.call_plsql("BEGIN dbms_debug.detach_session; END;", vec![], &[]).await;
        if !self.target_done() {
            let _ = tokio::time::timeout(Duration::from_secs(5), self.done.notified()).await;
        }
        let r = self.result.lock().unwrap().take();
        if r.is_none() {
            // 대상이 아직 붙잡혀 있다 — 기다리지 않고 끊는다 (서버에서 롤백된다)
            self.target.abandon();
            self.ctl.close();
            return None;
        }
        // COMMIT/ROLLBACK 은 PL/SQL 이 아니라 디버그 대상이 되지 않는다
        let _ = if commit { self.target.commit().await } else { self.target.rollback().await };
        self.target.close();
        self.ctl.close();
        r
    }

    /// 대상 블록의 결과를 지우지 않고 본다 (끝난 뒤 화면 표시용)
    pub fn result(&self) -> Option<std::result::Result<ExecResult, String>> {
        self.result.lock().unwrap().as_ref().map(|r| r.as_ref().map(|x| x.clone()).map_err(|e| e.to_string()))
    }
}

/// 디버그할 블록을 감싼다: 끝나거나 예외가 나면 같은 호출 안에서 DEBUG_OFF.
/// 줄 번호를 지키려고 `BEGIN` 을 첫 줄 앞에 붙인다 (첫 줄의 열 위치만 밀린다).
pub fn wrap_block(block: &str) -> String {
    let mut body = block.trim().trim_end_matches('/').trim_end().to_string();
    if !body.ends_with(';') {
        body.push(';');
    }
    format!("BEGIN {body}\n dbms_debug.debug_off; EXCEPTION WHEN OTHERS THEN dbms_debug.debug_off; RAISE; END;")
}

/// 권한이 없을 때 해결 방법을 붙인다
fn privilege_hint(e: Error) -> Error {
    match &e {
        Error::Db { code: 1031, .. } | Error::Db { code: 942, .. } | Error::Db { code: 6550, .. }
            if !matches!(&e, Error::Db { code: 6550, message, .. } if !(message.contains("PLS-00201") && message.to_uppercase().contains("DBMS_DEBUG"))) =>
        {
            Error::Invalid(format!(
            "{e}\n\n디버그 권한이 필요합니다: GRANT DEBUG CONNECT SESSION TO <사용자>; \
             (남의 단위는 GRANT DEBUG ANY PROCEDURE 또는 GRANT DEBUG ON <단위>)"
            ))
        }
        _ => e,
    }
}

async fn load_consts(ctl: &Session) -> Result<Consts> {
    let v = ctl.call_plsql(CONSTS_SQL, vec![], CONSTS_OUT).await.map_err(privilege_hint)?;
    let g = |n: &str| num(&v[CONSTS_OUT.iter().position(|x| *x == n).unwrap()]);
    let mut reasons = HashMap::new();
    for (n, label) in [
        ("r_bp", "breakpoint"),
        ("r_enter", "step"),
        ("r_return", "step"),
        ("r_line", "step"),
        ("r_finish", "step"),
        ("r_exc", "exception"),
        ("r_handler", "exception"),
        ("r_start", "start"),
        ("r_abort", "abort"),
    ] {
        reasons.insert(g(n), label);
    }
    let mut errors = HashMap::new();
    for (n, label) in [
        ("e_nosuch", "그런 변수가 없습니다 (범위 밖이거나 최적화로 사라짐)"),
        ("e_unknown", "스칼라가 아닌 형식입니다 (레코드·컬렉션·객체)"),
        ("e_nodebug", "디버그 정보가 없습니다 — 단위를 COMPILE DEBUG 로 다시 컴파일하세요"),
        ("e_badframe", "그런 호출 단계가 없습니다"),
        ("e_illegal", "실행할 수 없는 줄입니다 (주석·선언·빈 줄)"),
        ("e_badhandle", "단위를 찾을 수 없거나 디버그 정보가 없습니다"),
        ("e_indexed", "인덱스 테이블은 원소 단위로 읽어야 합니다 (예: t(1))"),
        ("e_nullcol", "컬렉션이 NULL 입니다"),
        ("e_timeout", "시간 초과"),
    ] {
        errors.insert(g(n), label);
    }
    let mut lu = HashMap::new();
    for (n, t) in [
        ("lu_proc", "PROCEDURE"),
        ("lu_func", "FUNCTION"),
        ("lu_pkg", "PACKAGE"),
        ("lu_body", "PACKAGE BODY"),
        ("lu_trig", "TRIGGER"),
        ("lu_cursor", "CURSOR"),
    ] {
        lu.insert(g(n), t);
    }
    Ok(Consts {
        b_next: g("b_next"),
        b_call: g("b_call"),
        b_ret: g("b_ret"),
        b_exc: g("b_exc"),
        abort: g("abort"),
        info: g("info"),
        success: g("success"),
        e_timeout: g("e_timeout"),
        exit_reasons: vec![g("r_exit"), g("r_knl")],
        reasons,
        ns_body: g("ns_body"),
        ns_top: g("ns_top"),
        ns_trigger: g("ns_trigger"),
        lu,
        errors,
    })
}

// ─────────────────────────────────────────────────────────────
// 소스에서 지역 변수 찾기 (DBMS_DEBUG 에는 "지역 변수 목록" 이 없다)
// ─────────────────────────────────────────────────────────────

/// `line`(1부터) 이 속한 서브프로그램의 매개변수와 선언부 변수 이름.
/// 패키지 본문이면 그 위의 패키지 전역 변수도 덧붙인다. 완벽할 필요는 없다 —
/// 없는 이름은 get_value 가 "없음" 으로 걸러 준다.
pub fn locals_at(source: &[String], line: usize) -> Vec<String> {
    let upto = line.min(source.len());
    let text: String = source[..upto].join("\n");
    let upper = text.to_uppercase();
    let mut out: Vec<String> = Vec::new();
    let push = |n: &str, out: &mut Vec<String>| {
        let n = n.trim().trim_matches('"').to_uppercase();
        if !n.is_empty() && n.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) && !RESERVED.contains(&n.as_str()) && !out.contains(&n) {
            out.push(n);
        }
    };

    // 마지막 PROCEDURE / FUNCTION 머리 (없으면 익명 블록의 DECLARE)
    let head = ["PROCEDURE ", "FUNCTION "]
        .iter()
        .filter_map(|k| upper.rfind(k))
        .max();
    let decl_start = match head {
        Some(h) => {
            // 매개변수 목록
            let after = &text[h..];
            if let (Some(o), Some(c)) = (after.find('('), find_close(after)) {
                if o < c && after[..o].lines().count() <= 2 {
                    for part in split_top(&after[o + 1..c]) {
                        if let Some(n) = part.split_whitespace().next() {
                            push(n, &mut out);
                        }
                    }
                }
            }
            // IS / AS 뒤
            let au = after.to_uppercase();
            let is = [" IS", "\nIS", " AS", "\nAS", ")IS", ")AS"].iter().filter_map(|k| au.find(k).map(|p| p + k.len())).min();
            is.map(|p| h + p)
        }
        None => upper.find("DECLARE").map(|p| p + 7),
    };
    if let Some(ds) = decl_start {
        // 선언부: BEGIN 전까지, "이름 형식...;" 꼴
        let body = &text[ds..];
        let bu = body.to_uppercase();
        let end = find_word(&bu, "BEGIN").unwrap_or(body.len());
        for stmt in body[..end].split(';') {
            let s = strip_comments(stmt);
            let mut w = s.split_whitespace();
            let Some(first) = w.next() else { continue };
            let fu = first.to_uppercase();
            if matches!(fu.as_str(), "CURSOR" | "TYPE" | "SUBTYPE" | "PRAGMA" | "PROCEDURE" | "FUNCTION") {
                continue;
            }
            push(first, &mut out);
        }
    }
    out
}

const RESERVED: &[&str] = &["BEGIN", "END", "IS", "AS", "DECLARE", "IN", "OUT", "NOCOPY", "RETURN", "EXCEPTION"];

fn find_word(hay: &str, w: &str) -> Option<usize> {
    let b = hay.as_bytes();
    let mut from = 0;
    while let Some(p) = hay[from..].find(w) {
        let i = from + p;
        let before = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        let j = i + w.len();
        let after = j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_');
        if before && after {
            return Some(i);
        }
        from = i + w.len();
    }
    None
}

fn find_close(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    let mut in_str = false;
    for c in s.chars() {
        if c == '\'' {
            in_str = !in_str;
        }
        if in_str {
            cur.push(c);
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn strip_comments(s: &str) -> String {
    s.lines()
        .map(|l| l.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn locals_in_package_procedure() {
        let s = src("CREATE OR REPLACE PACKAGE BODY p IS
  g_count NUMBER := 0;
  PROCEDURE a(p_x IN NUMBER) IS BEGIN NULL; END;
  PROCEDURE add_one(p_in IN NUMBER, p_out OUT NUMBER, p_opt IN VARCHAR2 DEFAULT 'a,b') IS
    v_tmp   NUMBER := p_in;   -- 임시
    v_name  VARCHAR2(30);
    CURSOR c IS SELECT 1 FROM dual;
    TYPE t_tab IS TABLE OF NUMBER;
  BEGIN
    v_tmp := v_tmp + 1;
    p_out := v_tmp;
  END;
END;");
        let l = locals_at(&s, 10);
        assert_eq!(l, ["P_IN", "P_OUT", "P_OPT", "V_TMP", "V_NAME"]);
    }

    #[test]
    fn wrapping_keeps_line_numbers() {
        let b = "DECLARE\n  r NUMBER;\nBEGIN\n  r := 1;\nEND;\n/";
        let w = wrap_block(b);
        assert!(w.starts_with("BEGIN DECLARE\n"));
        assert_eq!(w.lines().nth(3), Some("  r := 1;"), "4행은 4행 그대로");
        assert!(w.ends_with("RAISE; END;"));
        assert!(wrap_block("BEGIN p END").contains("BEGIN p END;"));
    }

    #[test]
    fn locals_in_anonymous_block() {
        let s = src("DECLARE\n  r NUMBER;\n  s VARCHAR2(10) := 'x';\nBEGIN\n  r := 1;\nEND;");
        assert_eq!(locals_at(&s, 5), ["R", "S"]);
        assert!(locals_at(&src("BEGIN\n  NULL;\nEND;"), 2).is_empty());
    }
}

// ─────────────────────────────────────────────────────────────
// 호출 블록 만들기 (ALL_ARGUMENTS) / 소스 / 디버그 컴파일
// ─────────────────────────────────────────────────────────────

/// 서브프로그램 인자 하나 (ALL_ARGUMENTS 의 한 줄, data_level 0)
#[derive(Debug, Clone, Serialize)]
pub struct Arg {
    pub name: Option<String>,
    pub position: u32,
    pub data_type: String,
    /// IN | OUT | IN/OUT
    pub in_out: String,
    pub type_name: Option<String>,
    pub type_owner: Option<String>,
}

/// 디버그로 부를 수 있는 서브프로그램 하나
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// 화면 표시: `PKG.PROC` (오버로드면 `PKG.PROC #2`)
    pub label: String,
    pub template: String,
}

fn local_type(a: &Arg) -> String {
    match a.data_type.as_str() {
        "VARCHAR2" | "CHAR" | "NVARCHAR2" | "NCHAR" | "LONG" => "VARCHAR2(32767)".into(),
        "NUMBER" | "FLOAT" | "BINARY_INTEGER" | "PLS_INTEGER" | "BINARY_DOUBLE" | "BINARY_FLOAT" => a.data_type.clone(),
        "DATE" | "CLOB" | "BLOB" | "BOOLEAN" | "TIMESTAMP" | "RAW" => {
            if a.data_type == "RAW" { "RAW(32767)".into() } else { a.data_type.clone() }
        }
        "REF CURSOR" => "SYS_REFCURSOR".into(),
        _ => match (&a.type_owner, &a.type_name) {
            (Some(o), Some(t)) => format!("{o}.{t}"),
            (None, Some(t)) => t.clone(),
            _ => format!("{} /* 형식을 확인하세요 */", a.data_type),
        },
    }
}

fn printable(a: &Arg) -> bool {
    matches!(
        a.data_type.as_str(),
        "VARCHAR2" | "CHAR" | "NVARCHAR2" | "NCHAR" | "NUMBER" | "FLOAT" | "BINARY_INTEGER" | "PLS_INTEGER" | "DATE" | "TIMESTAMP"
    )
}

/// `owner.pkg.sub(...)` 을 부르는 익명 블록. IN 은 바인드(:이름), OUT 은 지역 변수 + DBMS_OUTPUT.
pub fn call_template(owner: &str, package: Option<&str>, sub: &str, args: &[Arg]) -> String {
    let target = match package {
        Some(p) => format!("{owner}.{p}.{sub}"),
        None => format!("{owner}.{sub}"),
    };
    let ret = args.iter().find(|a| a.position == 0);
    let params: Vec<&Arg> = args.iter().filter(|a| a.position > 0 && a.name.is_some()).collect();
    let mut decl = Vec::new();
    let mut prints = Vec::new();
    if let Some(r) = ret {
        decl.push(format!("  v_return {};", local_type(r)));
        if printable(r) {
            prints.push("  DBMS_OUTPUT.PUT_LINE('return = ' || v_return);".to_string());
        } else if r.data_type == "BOOLEAN" {
            prints.push("  DBMS_OUTPUT.PUT_LINE('return = ' || CASE WHEN v_return THEN 'TRUE' WHEN NOT v_return THEN 'FALSE' END);".to_string());
        }
    }
    let mut call_args = Vec::new();
    for a in &params {
        let n = a.name.clone().unwrap();
        let lower = n.to_lowercase();
        let is_out = a.in_out != "IN";
        // BOOLEAN 은 바인드할 수 없다 — 지역 변수로
        if is_out || a.data_type == "BOOLEAN" || !printable(a) && a.data_type != "CLOB" {
            let init = if a.in_out == "IN" && a.data_type == "BOOLEAN" { " := TRUE" } else { "" };
            decl.push(format!("  {lower} {}{init};", local_type(a)));
            call_args.push(format!("    {n} => {lower}"));
            if is_out && printable(a) {
                prints.push(format!("  DBMS_OUTPUT.PUT_LINE('{lower} = ' || {lower});"));
            }
        } else {
            call_args.push(format!("    {n} => :{lower}"));
        }
    }
    let call = if call_args.is_empty() {
        format!("{target};")
    } else {
        format!("{target}(\n{}\n  );", call_args.join(",\n"))
    };
    let assign = if ret.is_some() { "v_return := " } else { "" };
    let mut out = String::new();
    if !decl.is_empty() {
        out.push_str("DECLARE\n");
        out.push_str(&decl.join("\n"));
        out.push('\n');
    }
    out.push_str("BEGIN\n");
    out.push_str(&format!("  {assign}{call}\n"));
    for p in prints {
        out.push_str(&p);
        out.push('\n');
    }
    out.push_str("END;");
    out
}

fn cell(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

/// 단위의 서브프로그램과 호출 블록. PACKAGE 면 공개 프로시저·함수 전부.
pub async fn entries(s: &Session, owner: &str, name: &str, unit_type: &str) -> Result<Vec<Entry>> {
    let is_pkg = unit_type.starts_with("PACKAGE");
    let sql = if is_pkg {
        "SELECT object_name, NVL(overload, '0'), argument_name, position, data_type, in_out, type_name, type_owner \
         FROM all_arguments WHERE owner = :o AND package_name = :n AND data_level = 0 \
         ORDER BY object_name, TO_NUMBER(NVL(overload, '0')), sequence"
    } else {
        "SELECT object_name, NVL(overload, '0'), argument_name, position, data_type, in_out, type_name, type_owner \
         FROM all_arguments WHERE owner = :o AND object_name = :n AND package_name IS NULL AND data_level = 0 \
         ORDER BY sequence"
    };
    let page = s
        .query(sql, vec![("O".into(), Some(owner.to_string())), ("N".into(), Some(name.to_string()))], 20_000)
        .await?;
    // (서브프로그램, 오버로드) 로 묶는다
    let mut groups: Vec<((String, String), Vec<Arg>)> = Vec::new();
    for r in &page.rows {
        let key = (cell(r, 0), cell(r, 1));
        let arg = Arg {
            name: r.get(2).cloned().flatten(),
            position: cell(r, 3).parse().unwrap_or(0),
            data_type: cell(r, 4),
            in_out: cell(r, 5),
            type_name: r.get(6).cloned().flatten(),
            type_owner: r.get(7).cloned().flatten(),
        };
        match groups.last_mut() {
            Some((k, v)) if *k == key => v.push(arg),
            _ => groups.push((key, vec![arg])),
        }
    }
    // 인자 없는 프로시저는 ALL_ARGUMENTS 에 "이름 없는 한 줄" 이 있거나 아예 없다
    if !is_pkg && groups.is_empty() {
        groups.push(((name.to_string(), "0".into()), vec![]));
    }
    let overloaded: std::collections::HashSet<String> = {
        let mut seen = std::collections::HashMap::<String, u32>::new();
        for ((n, _), _) in &groups {
            *seen.entry(n.clone()).or_default() += 1;
        }
        seen.into_iter().filter(|(_, c)| *c > 1).map(|(n, _)| n).collect()
    };
    Ok(groups
        .into_iter()
        .map(|((sub, ov), args)| {
            let args: Vec<Arg> = args.into_iter().filter(|a| a.name.is_some() || a.position == 0).collect();
            let label = match (is_pkg, overloaded.contains(&sub)) {
                (true, true) => format!("{name}.{sub} #{ov}"),
                (true, false) => format!("{name}.{sub}"),
                (false, _) => sub.clone(),
            };
            let template = if is_pkg {
                call_template(owner, Some(name), &sub, &args)
            } else {
                call_template(owner, None, &sub, &args)
            };
            Entry { label, template }
        })
        .collect())
}

/// ALL_SOURCE 의 줄들 (끝의 줄바꿈 없이). 줄 번호 = 인덱스 + 1.
pub async fn source(s: &Session, owner: &str, name: &str, unit_type: &str) -> Result<Vec<String>> {
    let page = s
        .query(
            "SELECT text FROM all_source WHERE owner = :o AND name = :n AND type = :t ORDER BY line",
            vec![
                ("O".into(), Some(owner.to_string())),
                ("N".into(), Some(name.to_string())),
                ("T".into(), Some(unit_type.to_string())),
            ],
            1_000_000,
        )
        .await?;
    Ok(page.rows.iter().map(|r| cell(r, 0).trim_end_matches(['\n', '\r']).to_string()).collect())
}

/// 디버그 정보로 컴파일되어 있는지 (ALL_PLSQL_OBJECT_SETTINGS.PLSQL_DEBUG). 모르면 None.
pub async fn has_debug_info(s: &Session, owner: &str, name: &str, unit_type: &str) -> Option<bool> {
    let page = s
        .query(
            "SELECT plsql_debug FROM all_plsql_object_settings WHERE owner = :o AND name = :n AND type = :t",
            vec![
                ("O".into(), Some(owner.to_string())),
                ("N".into(), Some(name.to_string())),
                ("T".into(), Some(unit_type.to_string())),
            ],
            1,
        )
        .await
        .ok()?;
    page.rows.first().map(|r| cell(r, 0) == "TRUE")
}

/// `ALTER ... COMPILE DEBUG` 문장 (사람이 확인한 뒤 실행한다)
pub fn compile_debug_sql(owner: &str, name: &str, unit_type: &str) -> Option<String> {
    let q = format!("\"{owner}\".\"{name}\"");
    Some(match unit_type {
        "PACKAGE" | "PACKAGE BODY" => format!("ALTER PACKAGE {q} COMPILE DEBUG"),
        "PROCEDURE" => format!("ALTER PROCEDURE {q} COMPILE DEBUG"),
        "FUNCTION" => format!("ALTER FUNCTION {q} COMPILE DEBUG"),
        "TRIGGER" => format!("ALTER TRIGGER {q} COMPILE DEBUG"),
        "TYPE" | "TYPE BODY" => format!("ALTER TYPE {q} COMPILE DEBUG BODY"),
        _ => return None,
    })
}

#[cfg(test)]
mod template_tests {
    use super::*;

    fn arg(name: &str, pos: u32, t: &str, io: &str) -> Arg {
        Arg { name: Some(name.into()), position: pos, data_type: t.into(), in_out: io.into(), type_name: None, type_owner: None }
    }

    #[test]
    fn procedure_with_in_and_out() {
        let t = call_template("SCOTT", Some("PKG"), "ADD_ONE", &[arg("P_IN", 1, "NUMBER", "IN"), arg("P_OUT", 2, "NUMBER", "OUT")]);
        assert_eq!(
            t,
            "DECLARE\n  p_out NUMBER;\nBEGIN\n  SCOTT.PKG.ADD_ONE(\n    P_IN => :p_in,\n    P_OUT => p_out\n  );\n  DBMS_OUTPUT.PUT_LINE('p_out = ' || p_out);\nEND;"
        );
    }

    #[test]
    fn function_and_boolean() {
        let ret = Arg { name: None, position: 0, data_type: "VARCHAR2".into(), in_out: "OUT".into(), type_name: None, type_owner: None };
        let t = call_template("SCOTT", None, "F", &[ret, arg("P_FLAG", 1, "BOOLEAN", "IN")]);
        assert!(t.contains("v_return VARCHAR2(32767);"));
        assert!(t.contains("p_flag BOOLEAN := TRUE;"), "{t}");
        assert!(t.contains("v_return := SCOTT.F("));
        assert!(t.contains("P_FLAG => p_flag"));
        assert!(t.contains("'return = ' || v_return"));
    }

    #[test]
    fn no_args() {
        assert_eq!(call_template("S", None, "P", &[]), "BEGIN\n  S.P;\nEND;");
    }

    #[test]
    fn compile_sql() {
        assert_eq!(compile_debug_sql("S", "P", "PACKAGE BODY").unwrap(), "ALTER PACKAGE \"S\".\"P\" COMPILE DEBUG");
        assert!(compile_debug_sql("S", "T", "TABLE").is_none());
    }
}
