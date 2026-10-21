//! 큰 분석 결과에서 화면이 쓰는 명령의 시간을 잰다 (수동):
//! `SQLS_PERF_UNITS=3000 cargo test -p sqls-analyze --release --test perf -- --ignored --nocapture`
//! 결과 폴더는 한 번 만들어 두고 다시 쓴다 (환경변수 SQLS_PERF_DIR, 기본 임시 폴더)

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use sqls_analyze::bundle;
use sqls_analyze::chunk::UnitSource;
use sqls_analyze::run::{analyze_unit, Event, Options};
use sqls_analyze::store::Store;

/// 패키지 본문처럼 — 프로시저 여럿, 테이블 여럿을 읽고 쓴다
fn source(i: usize) -> String {
    let mut s = format!("PACKAGE BODY pkg_{i} IS\n");
    for p in 0..8 {
        s.push_str(&format!("  PROCEDURE p{p}(a IN NUMBER) IS\n    v NUMBER;\n  BEGIN\n"));
        for t in 0..6 {
            let tb = (i * 7 + p * 3 + t) % 400;
            s.push_str(&format!("    SELECT COUNT(*) INTO v FROM tab_{tb} x WHERE x.id = a AND x.st = 'A';\n"));
            s.push_str(&format!("    UPDATE tab_{} SET amt = amt + v, upd_dt = SYSDATE WHERE id = a;\n", (tb + 1) % 400));
            s.push_str(&format!("    INSERT INTO hist_{} (id, amt) SELECT id, amt FROM tab_{tb} WHERE id = a;\n", tb % 50));
        }
        s.push_str(&format!("    pkg_{}.p{}(a);\n  END p{p};\n", (i + 1) % 1000, (p + 1) % 8));
    }
    s.push_str(&format!("END pkg_{i};"));
    s
}

fn time<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("{:>8.2}s  {what}", t.elapsed().as_secs_f64());
    r
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn big_set_timings() {
    let n: usize = std::env::var("SQLS_PERF_UNITS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
    let root = std::env::var("SQLS_PERF_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join(format!("sqls-perf-{n}")));
    let dir = root.join("BIG");
    let store = Store::open(&dir).unwrap();
    if store.units().len() < n {
        let t = Instant::now();
        for i in 0..n {
            let src = UnitSource { owner: "APP".into(), name: format!("PKG_{i}"), unit_type: "PACKAGE BODY".into(), text: source(i) };
            analyze_unit(&store, &src, None, None, &Options::default(), Arc::new(|_: Event| {}), Arc::new(AtomicBool::new(false))).await.unwrap();
        }
        sqls_analyze::integrate::write_all(&store, None).unwrap();
        println!("준비: 단위 {n}개 {:.1}s", t.elapsed().as_secs_f64());
    }
    let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) / 1024;
    println!("integrated.json {}KB · graph.json {}KB · statements.json {}KB", size(&dir.join("integrated/integrated.json")), size(&dir.join("integrated/graph.json")), size(&dir.join("integrated/statements.json")));

    let sets = time("analysis_sets (list_sets)", || bundle::list_sets(&root));
    assert!(sets.iter().any(|s| s.units >= n && s.generated_at.is_some()), "생성 시각을 머리에서 읽는다");
    let units = time("(전) store.units — 단위 파일 전부", || store.units());
    assert_eq!(units.len(), n);
    let _ = std::fs::remove_file(dir.join("integrated/units-index.json"));
    let b = time("(후) store.briefs — 색인 없음 (만들기)", || store.briefs());
    assert_eq!(b.len(), n);
    let b = time("(후) store.briefs — 색인 있음", || store.briefs());
    assert_eq!(b.len(), n);
    let g = time("analysis_result (read integrated.json)", || store.read_integrated::<sqls_analyze::integrate::Integrated>("integrated.json")).unwrap();
    let full = time("(전) 화면으로 보낼 JSON 만들기 — 전체", || serde_json::to_string(&g).unwrap());
    let slim = sqls_analyze::integrate::for_screen(g);
    let small = time("(후) 화면으로 보낼 JSON 만들기 — for_screen", || serde_json::to_string(&slim).unwrap());
    println!("          화면으로 보내는 양 {:.1}MB → {:.1}MB", full.len() as f64 / 1e6, small.len() as f64 / 1e6);

    // 지우기: 사본을 만들어 지운다
    let copy = root.join("COPY");
    let _ = std::fs::remove_dir_all(&copy);
    time("(사본 만들기)", || copy_tree(&dir, &copy));
    time("(전) remove_dir_all", || std::fs::remove_dir_all(&copy).unwrap());
    time("(사본 만들기)", || copy_tree(&dir, &copy));
    time("(후) remove_set_dir — 이름 바꾸고 뒤에서", || bundle::remove_set_dir(&copy).unwrap());
    assert!(!copy.exists());
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}
