//! 결과 저장 — 조각마다 JSON 파일 하나.
//!
//! ```text
//! <out>/
//!   units/<OWNER.NAME.TYPE>/unit.json            단위: 구조, 서브프로그램별 사실·요약, 단위 요약
//!   units/<OWNER.NAME.TYPE>/chunks/<id>.json     조각: 코드, 정적 사실, 모델 답, 호출 기록
//!   integrated/graph.json, crud.json, findings.json, report.md   통합 분석 (integrate)
//! ```
//!
//! - 파일은 임시 파일에 쓰고 이름을 바꾼다 (중간에 죽어도 반쯤 쓴 파일이 남지 않는다).
//! - 조각 파일의 `analysis_key` = 조각 해시 + 모델 + 프롬프트 버전. 같으면 다시 묻지 않는다 —
//!   멈췄다 다시 돌리면 이어서 하고, 소스가 바뀐 조각만 다시 분석한다.
//! - 사람이 읽고 다른 도구(jq, 스크립트, 다른 LLM)로 다시 쓰기 좋게 들여쓴 JSON 이다.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::chunk::Chunk;
use crate::facts::Facts;
use crate::llm::{CallMeta, Insight};
use crate::plsql::{Decl, SubKind};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkResult {
    pub format: u32,
    pub unit: String,
    pub chunk: Chunk,
    /// 모델 없이(정적 분석만) 돌렸으면 None
    pub analysis_key: Option<String>,
    pub insight: Option<Insight>,
    pub llm: Option<CallMeta>,
    pub error: Option<String>,
    /// 읽지 못한 답 (오류일 때만)
    pub raw: Option<String>,
    pub analyzed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubResult {
    pub path: String,
    pub name: String,
    pub overload: u32,
    pub kind: SubKind,
    pub signature: String,
    pub start_line: u32,
    pub end_line: u32,
    /// 명세에 선언되어 있는지 (명세를 못 봤으면 None)
    pub public: Option<bool>,
    pub facts: Facts,
    pub summary: Option<Insight>,
    pub chunk_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub chunks: u32,
    /// 이번에 모델에 물은 조각
    pub asked: u32,
    /// 이전 결과를 그대로 쓴 조각
    pub cached: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitResult {
    pub format: u32,
    pub key: String,
    pub owner: String,
    pub name: String,
    pub unit_type: String,
    pub lines: u32,
    pub hash: String,
    /// 패키지 명세의 공개 선언 (본문 분석 때 명세를 같이 읽었으면)
    pub public_decls: Vec<Decl>,
    pub warning: Option<String>,
    pub subprograms: Vec<SubResult>,
    /// 패키지 전역 선언의 사실
    pub globals: Facts,
    /// 단위 전체 사실 (전역 + 서브프로그램)
    pub facts: Facts,
    pub summary: Option<Insight>,
    pub chunk_ids: Vec<String>,
    pub stats: Stats,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// 요약 캐시 키 — 입력이 같으면 다시 묻지 않는다 ("path#overload" 또는 "_unit" → 키)
    #[serde(default)]
    pub rollup_keys: std::collections::BTreeMap<String, String>,
    pub analyzed_at: u64,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn safe_name(key: &str) -> String {
    key.chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '.' | '-' | '~') { c } else { '_' }).collect()
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("units"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn unit_dir(&self, key: &str) -> PathBuf {
        self.root.join("units").join(safe_name(key))
    }

    pub fn chunk_path(&self, key: &str, id: &str) -> PathBuf {
        self.unit_dir(key).join("chunks").join(format!("{}.json", safe_name(id)))
    }

    pub fn read_chunk(&self, key: &str, id: &str) -> Option<ChunkResult> {
        read_json(&self.chunk_path(key, id))
    }

    pub fn write_chunk(&self, r: &ChunkResult) -> io::Result<()> {
        write_json(&self.chunk_path(&r.unit, &r.chunk.id), r)
    }

    pub fn read_unit(&self, key: &str) -> Option<UnitResult> {
        read_json(&self.unit_dir(key).join("unit.json"))
    }

    /// 단위를 쓰고, 지금 계획에 없는 옛 조각 파일을 지운다 (소스가 바뀌어 조각이 달라졌을 때)
    pub fn write_unit(&self, u: &UnitResult) -> io::Result<()> {
        write_json(&self.unit_dir(&u.key).join("unit.json"), u)?;
        let dir = self.unit_dir(&u.key).join("chunks");
        if let Ok(rd) = fs::read_dir(&dir) {
            let keep: std::collections::HashSet<String> = u.chunk_ids.iter().map(|i| format!("{}.json", safe_name(i))).collect();
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.ends_with(".json") && !keep.contains(&n) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        Ok(())
    }

    /// 저장된 단위 전부 (통합 분석의 입력)
    pub fn units(&self) -> Vec<UnitResult> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(self.root.join("units")) {
            for e in rd.flatten() {
                if let Some(u) = read_json::<UnitResult>(&e.path().join("unit.json")) {
                    out.push(u);
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    pub fn chunks_of(&self, u: &UnitResult) -> Vec<ChunkResult> {
        u.chunk_ids.iter().filter_map(|id| self.read_chunk(&u.key, id)).collect()
    }

    pub fn write_integrated<T: Serialize>(&self, name: &str, v: &T) -> io::Result<PathBuf> {
        let p = self.root.join("integrated").join(name);
        write_json(&p, v)?;
        Ok(p)
    }

    pub fn write_text(&self, rel: &str, text: &str) -> io::Result<PathBuf> {
        let p = self.root.join(rel);
        write_atomic(&p, text.as_bytes())?;
        Ok(p)
    }

    pub fn read_integrated<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        read_json(&self.root.join("integrated").join(name))
    }
}

fn read_json<T: DeserializeOwned>(p: &Path) -> Option<T> {
    let text = fs::read_to_string(p).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("{} 을 읽을 수 없습니다: {e}", p.display());
            None
        }
    }
}

fn write_json<T: Serialize>(p: &Path, v: &T) -> io::Result<()> {
    let text = serde_json::to_string_pretty(v).map_err(io::Error::other)?;
    write_atomic(p, text.as_bytes())
}

fn write_atomic(p: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, p)
}
