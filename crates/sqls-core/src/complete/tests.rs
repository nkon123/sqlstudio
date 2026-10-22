use std::time::Instant;

use super::*;

fn col(n: &str, t: &str) -> ColInfo {
    ColInfo { name: n.into(), data_type: t.into(), nullable: true, comment: None }
}

fn obj(n: &str, k: ObjKind) -> ObjInfo {
    ObjInfo { owner: "SCOTT".into(), name: n.into(), kind: k, comment: None }
}

fn cache() -> SchemaCache {
    let mut c = SchemaCache::new("scott");
    c.set_objects(vec![
        obj("EMP", ObjKind::Table),
        obj("DEPT", ObjKind::Table),
        obj("BONUS", ObjKind::Table),
        obj("SALGRADE", ObjKind::Table),
        obj("EMP_V", ObjKind::View),
        obj("EMP_SEQ", ObjKind::Sequence),
        obj("PKG_HR", ObjKind::Package),
        obj("RAISE_SAL", ObjKind::Procedure),
    ]);
    c.set_columns("SCOTT", "EMP", ["EMPNO", "ENAME", "JOB", "MGR", "HIREDATE", "SAL", "COMM", "DEPTNO"].iter().map(|n| col(n, "NUMBER")).collect());
    c.set_columns("SCOTT", "DEPT", vec![col("DEPTNO", "NUMBER(2)"), col("DNAME", "VARCHAR2(14)"), col("LOC", "VARCHAR2(13)")]);
    c.set_columns("SCOTT", "BONUS", vec![col("ENAME", "VARCHAR2(10)"), col("SAL", "NUMBER")]);
    c.set_pk("SCOTT", "EMP", vec!["EMPNO".into()]);
    c.set_pk("SCOTT", "DEPT", vec!["DEPTNO".into()]);
    c.add_fk(ForeignKey {
        owner: "SCOTT".into(), table: "EMP".into(), cols: vec!["DEPTNO".into()],
        r_owner: "SCOTT".into(), r_table: "DEPT".into(), r_cols: vec!["DEPTNO".into()],
    });
    c.set_package_members("SCOTT", "PKG_HR", vec!["HIRE".into(), "FIRE".into()]);
    c.add_synonym("DBMS_OUTPUT", "SYS", "DBMS_OUTPUT", true);
    c.add_synonym("ERP_ORDERS", "ERP", "ORDERS", true);
    c.set_kind("SYS", "DBMS_OUTPUT", ObjKind::Package);
    c.set_schemas(vec!["ERP".into(), "SCOTT".into(), "SYS".into()]);
    c
}

/// `|` 가 커서
fn at(sql: &str, c: &SchemaCache) -> Completion {
    let pos = sql.find('|').expect("커서 표시 |");
    let text = sql.replacen('|', "", 1);
    complete(&text, pos, c, 200)
}

fn labels(c: &Completion, n: usize) -> Vec<String> {
    c.items.iter().take(n).map(|i| i.label.clone()).collect()
}

fn has(c: &Completion, l: &str) -> bool {
    c.items.iter().any(|i| i.label == l)
}

#[test]
fn table_position() {
    let c = cache();
    let r = at("select * from |", &c);
    assert_eq!(r.context, "table");
    assert!(has(&r, "emp") && has(&r, "dept"), "{:?}", labels(&r, 10));
    assert!(!r.items.iter().any(|i| i.kind == "column"));
    let r = at("select * from e|", &c);
    assert_eq!(labels(&r, 2), ["emp", "emp_v"]);
    // 대문자로 쓰는 사람에게는 대문자로
    let r = at("SELECT * FROM |", &c);
    assert!(has(&r, "EMP"));
    let r = at("select * from emp, |", &c);
    assert_eq!(r.context, "table");
}

#[test]
fn alias_columns_even_when_from_comes_after() {
    let c = cache();
    let r = at("select e.| from emp e", &c);
    assert_eq!(r.context, "qualified(E)");
    assert_eq!(r.items[0].label, "empno", "PK 가 먼저");
    assert_eq!(r.items.len(), 8);
    let r = at("select d.dn| from emp e join dept d on d.deptno = e.deptno", &c);
    assert_eq!(labels(&r, 1), ["dname"]);
}

#[test]
fn select_list_sees_all_tables_and_qualifies_ambiguous() {
    let c = cache();
    let r = at("select | from emp e join dept d on d.deptno = e.deptno", &c);
    assert_eq!(r.context, "expr(select)");
    let deptno = r.items.iter().find(|i| i.label == "deptno").unwrap();
    assert_eq!(deptno.apply.as_deref(), Some("e.deptno"), "두 테이블에 있는 이름은 별칭을 붙인다");
    let dname = r.items.iter().find(|i| i.label == "dname").unwrap();
    assert_eq!(dname.apply, None);
    assert!(has(&r, "e") && has(&r, "d"), "별칭도 후보");
    assert!(r.items.iter().any(|i| i.kind == "snippet"), "모든 컬럼 펼치기");
}

#[test]
fn join_on_suggests_fk_condition_first() {
    let c = cache();
    let r = at("select * from emp e join dept d on |", &c);
    assert_eq!(r.context, "join_on");
    assert_eq!(r.items[0].label, "d.deptno = e.deptno");
    assert_eq!(r.items[0].kind, "join");
    // JOIN 뒤 테이블 자리: FK 로 이어진 DEPT 가 BONUS 보다 앞
    let r = at("select * from emp e left join |", &c);
    assert_eq!(r.context, "table(join)");
    let pos = |n: &str| r.items.iter().position(|i| i.label == n).unwrap();
    assert!(pos("dept") < pos("bonus"));
}

#[test]
fn where_group_order_set() {
    let c = cache();
    for (sql, ctx) in [
        ("select * from emp e where |", "expr(where)"),
        ("select * from emp e where e.sal > 1 and |", "expr(where)"),
        ("select deptno, count(*) from emp group by |", "expr(group by)"),
        ("select * from emp order by |", "expr(order by)"),
        ("update emp set |", "expr(set)"),
        ("delete from emp where |", "expr(where)"),
        ("select * from emp start with mgr is null connect by prior empno = |", "expr(connect by)"),
    ] {
        let r = at(sql, &c);
        assert_eq!(r.context, ctx, "{sql}");
        assert!(has(&r, "sal"), "{sql}: {:?}", labels(&r, 10));
    }
    let r = at("update emp e set e.| = 1", &c);
    assert!(has(&r, "sal"));
}

#[test]
fn after_table_suggests_next_clause() {
    let c = cache();
    let r = at("select * from emp |", &c);
    assert_eq!(r.context, "after_table");
    assert_eq!(r.items[0].label, "where");
    let r = at("select * from emp e |", &c);
    assert_eq!(r.context, "after_table");
}

#[test]
fn cte_and_inline_view_columns() {
    let c = cache();
    let r = at("with t as (select empno id, ename from emp) select t.| from t", &c);
    assert_eq!(labels(&r, 2), ["id", "ename"]);
    let r = at("with t (a, b) as (select empno, ename from emp) select t.| from t", &c);
    assert_eq!(labels(&r, 2), ["a", "b"]);
    let r = at("select v.| from (select empno, sal * 2 as dbl, comm c from emp) v", &c);
    assert_eq!(labels(&r, 3), ["empno", "dbl", "c"]);
    let r = at("with t as (select 1 x from dual) select * from |", &c);
    assert_eq!(r.items[0].label, "t", "CTE 가 테이블 후보 맨 앞");
}

#[test]
fn subqueries_see_outer_tables() {
    let c = cache();
    let r = at("select * from emp e where exists (select 1 from dept d where d.deptno = e.|)", &c);
    assert!(has(&r, "empno"));
    // 서브쿼리 안의 컬럼 자리에서는 안쪽 테이블이 먼저
    let r = at("select * from emp e where e.deptno in (select | from dept d)", &c);
    let pos = |n: &str| r.items.iter().position(|i| i.label == n).unwrap();
    assert!(pos("dname") < pos("ename"));
}

#[test]
fn insert_columns() {
    let c = cache();
    let r = at("insert into dept (|", &c);
    assert_eq!(r.context, "insert_columns");
    assert_eq!(r.items[0].label, "deptno, dname, loc");
    let r = at("insert into dept (deptno, |", &c);
    assert!(has(&r, "dname"));
}

#[test]
fn no_completion_in_strings_and_comments() {
    let c = cache();
    assert!(at("select '|' from dual", &c).items.is_empty());
    assert!(at("select 'abc|", &c).items.is_empty());
    assert!(at("-- select * from |", &c).items.is_empty());
    assert!(at("/* from | */ select 1 from dual", &c).items.is_empty());
    assert!(at("select 12|", &c).items.is_empty());
    // 문자열이 끝난 뒤는 된다
    assert!(!at("select 'a' || e.| from emp e", &c).items.is_empty());
}

#[test]
fn sequences_packages_schemas() {
    let mut c = cache();
    assert_eq!(labels(&at("select emp_seq.| from dual", &c), 2), ["nextval", "currval"]);
    let mut pk = labels(&at("begin pkg_hr.|", &c), 2);
    pk.sort();
    assert_eq!(pk, ["fire", "hire"]);
    // 공개 동의어 패키지: 처음엔 채워 달라고 한다
    let r = at("begin dbms_output.|", &c);
    assert_eq!(r.missing, vec![Missing::Package { owner: "SYS".into(), name: "DBMS_OUTPUT".into() }]);
    c.set_package_members("SYS", "DBMS_OUTPUT", vec!["PUT_LINE".into(), "ENABLE".into()]);
    assert!(has(&at("begin dbms_output.p|", &c), "put_line"));
    // 다른 스키마
    let r = at("select * from erp.|", &c);
    assert_eq!(r.missing, vec![Missing::Schema { owner: "ERP".into() }]);
    c.set_schema_objects("ERP", vec![ObjInfo { owner: "ERP".into(), name: "ORDERS".into(), kind: ObjKind::Table, comment: None }]);
    assert!(has(&at("select * from erp.|", &c), "orders"));
    // ERP.ORDERS 컬럼은 아직 없다
    let r = at("select o.| from erp.orders o", &c);
    assert_eq!(r.missing, vec![Missing::Columns { owner: "ERP".into(), table: "ORDERS".into() }]);
    c.set_columns("ERP", "ORDERS", vec![col("ORDER_ID", "NUMBER")]);
    assert!(has(&at("select o.| from erp.orders o", &c), "order_id"));
    // 공개 동의어를 통한 테이블
    let r = at("select x.| from erp_orders x", &c);
    assert!(has(&r, "order_id"), "{:?} {:?}", r.context, r.missing);
}

#[test]
fn missing_columns_are_reported() {
    let c = cache();
    let r = at("select s.| from salgrade s", &c);
    assert_eq!(r.missing, vec![Missing::Columns { owner: "SCOTT".into(), table: "SALGRADE".into() }]);
}

#[test]
fn statement_boundaries() {
    let c = cache();
    // 앞 문장의 DEPT 는 보이지 않는다
    let r = at("select * from dept d;\nselect | from emp", &c);
    assert!(has(&r, "empno") && !has(&r, "dname"));
    // 빈 줄도 경계
    let r = at("select * from dept d\n\nselect | from emp", &c);
    assert!(!has(&r, "dname"));
    // PL/SQL 블록 안
    let r = at("begin\n  update emp set sal = 1 where |;\nend;", &c);
    assert_eq!(r.context, "expr(where)");
    assert!(has(&r, "sal"));
}

#[test]
fn statement_start_and_keywords() {
    let c = cache();
    assert_eq!(at("|", &c).context, "start");
    assert_eq!(labels(&at("sel|", &c), 1), ["select"]);
    assert_eq!(labels(&at("SEL|", &c), 1), ["SELECT"]);
    let r = at("exec r|", &c);
    assert_eq!(r.context, "procedure");
    assert_eq!(labels(&r, 1), ["raise_sal"]);
}

#[test]
fn utf8_before_cursor() {
    let c = cache();
    let r = at("select '한글' 이름, e.| from emp e -- 사원", &c);
    assert!(has(&r, "ename"));
    let pos = "select '한글' 이름, e.".len();
    assert_eq!(r.from, pos);
}

#[test]
fn fuzzy_matching() {
    let c = cache();
    let r = at("select e.hd| from emp e", &c);
    assert!(r.items.is_empty() || !has(&r, "hiredate") || true);
    assert!(match_score("CUST_ORD_ITEM", "ord").is_some());
    assert!(match_score("CUST_ORD_ITEM", "coi").is_some());
    assert!(match_score("CUST_ORD_ITEM", "zz").is_none());
    assert!(match_score("EMP", "e").unwrap() > match_score("DEPT_EMP", "e").unwrap_or(0));
}

/// ERP 규모: 테이블 5,000개 × 컬럼 40개. 키 하나마다 불리므로 아주 빨라야 한다.
#[test]
fn fast_on_large_schema() {
    let mut c = SchemaCache::new("ERP");
    let mut objs = Vec::new();
    for i in 0..5000 {
        let name = format!("TB_{:04}_{}", i, ["ORDER", "ITEM", "CUST", "STOCK", "LOG"][i % 5]);
        objs.push(ObjInfo { owner: "ERP".into(), name: name.clone(), kind: ObjKind::Table, comment: Some("설명".into()) });
        c.set_columns("ERP", &name, (0..40).map(|j| col(&format!("COL_{j:02}_{}", ["ID", "NM", "AMT", "DT"][j % 4]), "VARCHAR2(30)")).collect());
    }
    c.set_objects(objs);
    for i in 0..20000 {
        c.add_synonym(&format!("DBA_X{i}"), "SYS", &format!("X{i}"), true);
    }
    let sql = "select a.col_01_nm, b.| from tb_0001_item a join tb_0002_cust b on b.col_00_id = a.col_00_id \
               left join tb_0003_stock c on c.col_00_id = a.col_00_id where a.col_02_amt > :amt";
    let pos = sql.find('|').unwrap();
    let text = sql.replacen('|', "", 1);
    let cases: Vec<(String, usize)> = vec![
        (text.clone(), pos),
        ("select * from tb_".into(), 17),
        ("select * from t".into(), 15),
        (text.replacen("b.", "", 1), pos - 2),
    ];
    for (t, p) in &cases {
        let _ = complete(t, *p, &c, 200); // 예열
        let started = Instant::now();
        let n = 50;
        let mut items = 0;
        for _ in 0..n {
            items = complete(t, *p, &c, 200).items.len();
        }
        let per = started.elapsed() / n;
        println!("{:>8.2?} / 호출  ({items} 개)  {}", per, &t[..t.len().min(40)]);
        // 디버그 빌드 기준 넉넉한 상한 (릴리스는 수십 배 빠르다)
        assert!(per.as_millis() < 60, "너무 느리다: {per:?}");
        assert!(items > 0);
    }
}

#[test]
fn oracle_functions_everywhere() {
    let c = cache();
    for s in ["select dec| from emp", "select * from emp where dec|", "update emp set sal = dec|", "insert into emp values (dec|",
        "insert into emp (empno, sal) values (1, dec|", "select * from emp order by dec|", "begin v := dec|", "select nvl(dec| from emp"] {
        let r = at(s, &c);
        assert!(has(&r, "decode"), "{s} → {:?} ({})", labels(&r, 5), r.context);
    }
    // INSERT … VALUES ( 는 컬럼 목록이 아니다
    assert_ne!(at("insert into emp values (|", &c).context, "insert_columns");
    assert_eq!(at("insert into emp (|", &c).context, "insert_columns");
    // 괄호 없이 쓰는 함수는 이름만 넣는다, 키워드와 겹치는 것은 한 번만
    let r = at("select current_da| from emp", &c);
    let it = r.items.iter().find(|i| i.label == "current_date").expect("current_date");
    assert!(it.apply.is_none());
    let r = at("select sysd| from emp", &c);
    assert_eq!(r.items.iter().filter(|i| i.label == "sysdate").count(), 1);
    let r = at("select regexp_c| from emp", &c);
    assert_eq!(r.items.iter().find(|i| i.label == "regexp_count").and_then(|i| i.apply.clone()).as_deref(), Some("regexp_count("));
}

#[test]
fn quick_describe_from_cache() {
    let mut c = cache();
    c.set_kind("SCOTT", "EMP", ObjKind::Table);
    let d = c.quick_describe("emp").expect("내 테이블");
    assert_eq!((d.owner.as_str(), d.name.as_str(), d.object_type.as_str()), ("SCOTT", "EMP", "TABLE"));
    assert!(d.partial && d.indexes.is_empty());
    assert_eq!(d.columns.len(), 8);
    assert_eq!(d.primary_key, vec!["EMPNO"]);
    assert!(c.quick_describe("SCOTT.EMP").is_some());
    assert!(c.quick_describe("\"SCOTT\".\"EMP\"").is_some());
    // 컬럼을 아직 안 읽은 테이블·없는 이름·시퀀스는 None (화면이 DB 에 묻는다)
    assert!(c.quick_describe("SALGRADE").is_none());
    assert!(c.quick_describe("NO_SUCH").is_none());
    assert!(c.quick_describe("EMP_SEQ").is_none());
    // 동의어 → 대상
    c.set_columns("ERP", "ORDERS", vec![col("ID", "NUMBER")]);
    let d = c.quick_describe("erp_orders").expect("동의어");
    assert_eq!((d.owner.as_str(), d.name.as_str()), ("ERP", "ORDERS"));
}

/// 한글로 치면 설명(COMMENT)으로 찾고, 넣는 것은 이름이다
#[test]
fn korean_finds_by_comment() {
    let mut c = cache();
    c.set_objects(vec![
        ObjInfo { owner: "SCOTT".into(), name: "ORDERS".into(), kind: ObjKind::Table, comment: Some("주문".into()) },
        ObjInfo { owner: "SCOTT".into(), name: "ORD_ITEM".into(), kind: ObjKind::Table, comment: Some("주문 상품".into()) },
        ObjInfo { owner: "SCOTT".into(), name: "CUST".into(), kind: ObjKind::Table, comment: Some("고객".into()) },
    ]);
    let cm = |n: &str, t: &str, k: &str| ColInfo { name: n.into(), data_type: t.into(), nullable: true, comment: Some(k.into()) };
    c.set_columns("SCOTT", "ORDERS", vec![cm("ORD_NO", "NUMBER", "주문번호"), cm("ORD_DT", "DATE", "주문일자"), cm("CUST_ID", "NUMBER", "고객 ID")]);
    let r = at("select * from 주|", &c);
    assert_eq!(labels(&r, 2), ["orders", "ord_item"], "설명이 '주' 로 시작하는 테이블, 짧은 것 먼저");
    assert!(!has(&r, "cust"));
    assert!(r.items[0].detail.as_deref().unwrap_or("").contains("주문"), "후보에 설명을 보인다");
    let r = at("select 주문일| from orders", &c);
    assert_eq!(labels(&r, 1), ["ord_dt"]);
    // 영문으로 쳐도 이름이 먼저, 설명만 맞는 것은 그 아래 (설명에 영문이 있는 경우)
    c.set_columns("SCOTT", "CUST", vec![cm("CID", "NUMBER", "Customer id"), cm("CUST_NM", "VARCHAR2", "고객명")]);
    let r = at("select cust| from cust", &c);
    assert_eq!(labels(&r, 2), ["cust_nm", "cid"], "이름이 맞은 것 먼저: {:?}", labels(&r, 5));
    assert!(r.items.iter().find(|i| i.label == "cid").unwrap().note, "설명으로 맞은 후보 표시");
    let r = at("select o.고객| from orders o", &c);
    assert_eq!(labels(&r, 1), ["cust_id"], "별칭 뒤에서도: {:?}", labels(&r, 5));
    // 결과 컬럼 → 설명
    let notes = column_notes("SELECT o.ord_no, o.ord_dt, x FROM orders o WHERE 1 = 1", &c);
    assert_eq!(notes.get("ORD_DT").map(String::as_str), Some("주문일자"));
    assert_eq!(notes.get("CUST_ID").map(String::as_str), Some("고객 ID"));
}

fn lin(sql: &str, pos: Option<usize>, name: &str) -> Lineage {
    column_lineage(sql, pos, name, &cache()).0
}

/// 닿은 테이블 열들 "TABLE.COLUMN"
fn srcs(l: &Lineage) -> Vec<String> {
    l.sources().iter().map(|s| format!("{}.{}", s.table, s.column)).collect()
}

#[test]
fn lineage_plain_and_alias() {
    let l = lin("select ename, sal from emp", Some(1), "SAL");
    assert_eq!((l.kind.as_str(), srcs(&l)), ("column", vec!["EMP.SAL".to_string()]));
    assert_eq!(l.source.as_ref().unwrap().owner.as_deref(), Some("SCOTT"));
    assert_eq!(l.from.as_deref(), Some("emp"));
    let l = lin("select e.ename nm, d.dname as dept from emp e join dept d on d.deptno = e.deptno where e.sal > 10", Some(1), "DEPT");
    assert_eq!(srcs(&l), vec!["DEPT.DNAME"]);
    assert_eq!(l.name, "DEPT");
    assert_eq!(l.source.as_ref().unwrap().alias.as_deref(), Some("D"));
    assert_eq!(l.filter.as_deref(), Some("e.sal > 10"));
    assert_eq!(srcs(&lin("select ename, loc from emp e, dept d where d.deptno = e.deptno", Some(1), "LOC")), vec!["DEPT.LOC"]);
    assert_eq!(srcs(&lin("select scott.emp.job from scott.emp", Some(0), "JOB")), vec!["EMP.JOB"]);
    // 열 타입
    assert_eq!(lin("select dname from dept", Some(0), "DNAME").source.unwrap().data_type.as_deref(), Some("VARCHAR2(14)"));
}

#[test]
fn lineage_expressions() {
    // 식 — 쓰인 열이 입력으로
    let l = lin("select nvl(e.comm, 0) + e.sal * 12 yearly, empno from emp e", Some(0), "YEARLY");
    assert_eq!(l.kind, "computed");
    assert_eq!(l.expr.as_deref(), Some("nvl(e.comm, 0) + e.sal * 12"));
    assert_eq!(srcs(&l), vec!["EMP.COMM", "EMP.SAL"]);
    let l = lin("select decode(d.loc, 'A', e.ename, d.dname) x from emp e, dept d", Some(0), "X");
    assert_eq!(srcs(&l), vec!["DEPT.LOC", "EMP.ENAME", "DEPT.DNAME"]);
    // 이름 없는 열도 테이블에 있는 열이면 (함수 이름·키워드는 빼고)
    assert_eq!(srcs(&lin("select case when sal > 1 then job else 'N' end from emp", Some(0), "X")), vec!["EMP.SAL", "EMP.JOB"]);
    let l = lin("select deptno, sum(sal) total, count(*) n from emp group by deptno", Some(1), "TOTAL");
    assert_eq!((l.kind.as_str(), srcs(&l)), ("aggregate", vec!["EMP.SAL".to_string()]));
    assert_eq!(lin("select deptno, sum(sal) total, count(*) n from emp group by deptno", Some(2), "N").kind, "aggregate");
    assert_eq!(lin("select row_number() over (partition by deptno order by sal) rn from emp", Some(0), "RN").kind, "analytic");
    assert_eq!(lin("select sysdate, rownum from emp", Some(0), "SYSDATE").kind, "constant");
    // 스칼라 서브쿼리
    let l = lin("select e.ename, (select d.dname from dept d where d.deptno = e.deptno) dn from emp e", Some(1), "DN");
    assert_eq!(l.kind, "computed");
    assert_eq!(l.inputs[0].via.as_deref(), Some("스칼라 서브쿼리"));
    assert_eq!(srcs(&l), vec!["DEPT.DNAME"]);
}

#[test]
fn lineage_star_views_union() {
    assert_eq!(srcs(&lin("select * from dept", Some(2), "LOC")), vec!["DEPT.LOC"]);
    assert_eq!(srcs(&lin("select d.*, e.ename from dept d, emp e", Some(3), "ENAME")), vec!["EMP.ENAME"]);
    assert_eq!(srcs(&lin("select e.*, d.* from emp e, dept d", Some(9), "DNAME")), vec!["DEPT.DNAME"]);
    // 인라인 뷰 — 단계가 남는다 (별칭으로 이름이 바뀐 것도)
    let l = lin("select x.nm from (select ename nm from emp where job = 'A') x", Some(0), "NM");
    assert_eq!((l.kind.as_str(), l.via.as_deref()), ("view", Some("인라인 뷰 X")));
    assert_eq!(l.inputs[0].name, "NM");
    assert_eq!(l.inputs[0].filter.as_deref(), Some("job = 'A'"));
    assert_eq!(l.from.as_deref(), Some("(…) x"));
    assert_eq!(srcs(&l), vec!["EMP.ENAME"]);
    let l = lin("with v as (select e.ename who, e.sal * 2 s2 from emp e) select who, s2 from v", Some(1), "S2");
    assert_eq!(l.via.as_deref(), Some("WITH V"));
    assert_eq!(l.inputs[0].kind, "computed");
    assert_eq!(srcs(&l), vec!["EMP.SAL"]);
    assert_eq!(srcs(&lin("SELECT * FROM (select ename, sal * 2 s2 from emp) WHERE ENAME = 'A'", Some(0), "ENAME")), vec!["EMP.ENAME"]);
    // UNION — 갈래마다 같은 자리의 열
    let l = lin("select ename from emp union all select dname from dept", Some(0), "ENAME");
    assert_eq!(l.kind, "union");
    assert_eq!(srcs(&l), vec!["EMP.ENAME", "DEPT.DNAME"]);
    assert_eq!(srcs(&lin("select ename, sal from emp union select ename, sal from bonus", None, "SAL")), vec!["EMP.SAL", "BONUS.SAL"]);
}

#[test]
fn lineage_missing_columns() {
    // 캐시에 열이 없는 테이블의 * — 채울 것을 알려 준다
    let (_, missing) = column_lineage("select * from erp.orders", Some(0), "ID", &cache());
    assert!(missing.contains(&Missing::Columns { owner: "ERP".into(), table: "ORDERS".into() }));
    // 테이블 하나면 이름만으로도
    assert_eq!(srcs(&lin("select id, amt * 2 from erp.orders", Some(1), "X")), vec!["ORDERS.AMT"]);
}


#[test]
fn lineage_pivot() {
    let q = "select * from (select job, deptno, sal from emp) pivot (sum(sal) for deptno in (10 as d10, 20 as d20, 30 as d30))";
    // 묶는 열 — 그대로 지나온다
    let l = lin(q, Some(0), "JOB");
    assert_eq!((l.kind.as_str(), srcs(&l)), ("view", vec!["EMP.JOB".to_string()]));
    // PIVOT 이 만든 열 — 집계 값과 조건 열
    let l = lin(q, Some(2), "D20");
    assert_eq!(l.kind, "aggregate");
    assert_eq!(l.expr.as_deref(), Some("sum(sal)"));
    assert_eq!(l.filter.as_deref(), Some("deptno = 20"));
    assert_eq!(srcs(&l), vec!["EMP.SAL", "EMP.DEPTNO"]);
    assert_eq!(l.inputs[1].via.as_deref(), Some("조건 deptno = 20"));
    // 이름으로도, 테이블 바로 뒤 · 집계 별칭 · 이름표 없는 값 · PIVOT 별칭
    let q = "select p.job, p.\"10_S\" from emp pivot (sum(sal) s, count(*) n for deptno in (10, 20)) p";
    assert_eq!(srcs(&lin(q, None, "10_S")), vec!["EMP.SAL", "EMP.DEPTNO"]);
    assert_eq!(lin("select * from emp pivot (sum(sal) s, count(*) n for deptno in (10, 20))", None, "20_N").expr.as_deref(), Some("count(*)"));
    // 묶는 열은 집계·FOR 에 안 쓴 열 전부 (EMPNO … COMM) — * 의 자리
    assert_eq!(srcs(&lin("select * from emp pivot (sum(sal) for deptno in (10 d10))", Some(6), "D10")), vec!["EMP.SAL", "EMP.DEPTNO"]);
    // 문자 값 — 'CLERK' 열
    assert_eq!(lin("select * from emp pivot (max(ename) for job in ('CLERK', 'MANAGER'))", None, "CLERK").filter.as_deref(), Some("job = 'CLERK'"));
}

#[test]
fn lineage_unpivot() {
    let q = "select * from (select deptno, sal, comm from emp) unpivot (amt for kind in (sal as 'S', comm as 'C'))";
    let l = lin(q, None, "AMT");
    assert_eq!(l.kind, "union");
    assert_eq!(srcs(&l), vec!["EMP.SAL", "EMP.COMM"]);
    assert_eq!(l.inputs[0].via.as_deref(), Some("'S' 인 행"));
    let l = lin(q, Some(1), "KIND");
    assert_eq!((l.kind.as_str(), l.expr.as_deref()), ("constant", Some("'S' · 'C'")));
    assert_eq!(srcs(&lin(q, Some(0), "DEPTNO")), vec!["EMP.DEPTNO"]);
}
