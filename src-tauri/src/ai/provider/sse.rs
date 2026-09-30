//! Incremental Server-Sent Events parsing.
//!
//! Only `data:` fields matter to model endpoints; `event:` names are kept so
//! protocols that rely on them can read them. Comments and ids are ignored.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    /// Feeds raw bytes and returns every event they complete.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&String::from_utf8_lossy(&line), &mut events);
        }
        events
    }

    /// Flushes an event left open when the stream ends without a blank line.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
            self.handle_line(line.trim_end_matches('\r'), &mut events);
        }
        self.dispatch(&mut events);
        events
    }

    fn handle_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(events);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => self.data.push(value.to_string()),
            "event" => self.event = Some(value.to_string()),
            _ => {}
        }
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        if self.data.is_empty() {
            self.event = None;
            return;
        }
        events.push(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks_and_line_endings() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: {\"a\"").is_empty());
        let events = parser.push(b":1}\r\n\r\n: keep-alive\n\nevent: done\ndata: x\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "{\"a\":1}".into()
            }]
        );
        assert_eq!(
            parser.finish(),
            vec![SseEvent {
                event: Some("done".into()),
                data: "x".into()
            }]
        );
    }

    #[test]
    fn multibyte_text_survives_a_split_inside_a_character() {
        let bytes = "data: 你好\n\n".as_bytes();
        let mut parser = SseParser::default();
        assert!(parser.push(&bytes[..8]).is_empty());
        assert_eq!(parser.push(&bytes[8..])[0].data, "你好");
    }
}
