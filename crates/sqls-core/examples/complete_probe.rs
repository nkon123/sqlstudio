// 실제 사전으로 캐시를 채우고 자동완성을 찍어 본다:
//   SQLS_TEST_DSN=... cargo run -p sqls-core --example complete_probe -- "select * from t_ord o join t_cust c on |"
use sqls_core::complete::{self, load, SchemaCache};
use sqls_core::{ConnectSpec, Session};

#[tokio::main]
async fn main() {
    let dsn = std::env::var("SQLS_TEST_DSN").unwrap();
    let (cred, cs) = dsn.split_once('@').unwrap();
    let (u, p) = cred.split_once('/').unwrap();
    let mut spec = ConnectSpec::new(u, p, cs);
    spec.read_only = true;
    let s = Session::connect(spec).await.unwrap();
    let shared = std::sync::RwLock::new(SchemaCache::default());
    let mut cache = load::load_user_schema(&s, &shared, false, |_| {}).await.unwrap();
    for arg in std::env::args().skip(1) {
        let pos = arg.find('|').unwrap();
        let text = arg.replacen('|', "", 1);
        let mut r = complete::complete(&text, pos, &cache, 15);
        for m in r.missing.clone() {
            for f in load::fill(&s, &m).await.unwrap() {
                cache.apply(f);
            }
            r = complete::complete(&text, pos, &cache, 15);
        }
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(complete::complete(&text, pos, &cache, 200));
        }
        println!("{arg}\n  context={} missing={:?}  {:?}/호출", r.context, r.missing, t.elapsed() / 1000);
        for i in &r.items {
            println!("    {:<40} {:<9} {}", i.label, i.kind, i.detail.clone().unwrap_or_default());
        }
    }
}
