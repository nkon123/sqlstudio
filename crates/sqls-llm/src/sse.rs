//! 스트리밍 응답 파서 — SSE(`data: ...`) 와 NDJSON(줄마다 JSON).
//!
//! 바이트 단위로 버퍼링한다. 청크 경계가 UTF-8 한 글자 중간에 걸려도
//! (한글은 3바이트) 글자가 깨지지 않는다.

/// SSE 이벤트 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 청크를 넣고 완성된 이벤트들을 꺼낸다.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.is_empty() {
                if let Some(ev) = self.take() {
                    out.push(ev);
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // 주석 / keep-alive
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = Some(value.to_string()),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        out
    }

    /// 스트림이 끝났을 때 남은 이벤트 (마지막 빈 줄이 없는 서버 대비)
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let mut evs = self.push(&rest);
            evs.extend(self.push(b"\n"));
            if let Some(e) = evs.pop() {
                return Some(e);
            }
        }
        self.take()
    }

    fn take(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        let ev = SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
        };
        Some(ev)
    }
}

/// 줄 단위 JSON (Ollama `/api/chat`)
#[derive(Default)]
pub struct LineParser {
    buf: Vec<u8>,
}

impl LineParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line).trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
        }
        out
    }

    pub fn finish(&mut self) -> Option<String> {
        let s = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_across_chunks_and_utf8_split() {
        let raw = "event: content_block_delta\ndata: {\"t\":\"안녕\"}\n\n: ping\n\ndata: [DONE]\n\n";
        let bytes = raw.as_bytes();
        // 모든 가능한 위치에서 둘로 나눠 넣어도 결과가 같아야 한다
        for cut in 0..bytes.len() {
            let mut p = SseParser::new();
            let mut evs = p.push(&bytes[..cut]);
            evs.extend(p.push(&bytes[cut..]));
            assert_eq!(evs.len(), 2, "cut={cut}");
            assert_eq!(evs[0].event.as_deref(), Some("content_block_delta"));
            assert_eq!(evs[0].data, "{\"t\":\"안녕\"}");
            assert_eq!(evs[1].data, "[DONE]");
        }
    }

    #[test]
    fn crlf_and_multiline_data() {
        let mut p = SseParser::new();
        let evs = p.push(b"data: a\r\ndata: b\r\n\r\n");
        assert_eq!(evs[0].data, "a\nb");
    }

    #[test]
    fn finish_without_trailing_blank_line() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: x").is_empty());
        assert_eq!(p.finish().unwrap().data, "x");
    }

    #[test]
    fn ndjson() {
        let mut p = LineParser::default();
        let mut v = p.push("{\"a\":\"가".as_bytes());
        v.extend(p.push("\"}\n{\"b\":1}".as_bytes()));
        assert_eq!(v, vec!["{\"a\":\"가\"}"]);
        assert_eq!(p.finish().unwrap(), "{\"b\":1}");
    }
}
