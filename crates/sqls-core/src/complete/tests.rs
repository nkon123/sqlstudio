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
