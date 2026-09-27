//! Phone terminal snapshots, serialized without owning copies of their fields.

pub struct Frame<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub status: &'a str,
    pub label: &'a str,
    pub title: Option<&'a str>,
    pub tag: u8,
    pub tag_color: Option<&'a str>,
    pub ansi: Option<&'a str>,
    pub columns: Option<u16>,
    pub rows: Option<u16>,
    pub ansi_only: bool,
    pub updated_at: &'a str,
}

impl Frame<'_> {
    /// Reuse the connection's output allocation. Missing captures retain explicit
    /// nulls; `ansiOnly` omits only the plain tail of a present capture.
    pub fn write_json<'out>(&self, output: &'out mut Vec<u8>) -> &'out [u8] {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Document<'a> {
            // Keep the ordering of the earlier JSON object as well as its values.
            agent_type: &'a str,
            columns: Option<u16>,
            id: &'a str,
            label: &'a str,
            rows: Option<u16>,
            session_title: Option<&'a str>,
            status: &'a str,
            tag: u8,
            tag_color: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            tail: Option<Option<&'a str>>,
            tail_ansi: Option<&'a str>,
            tail_format: Option<&'a str>,
            updated_at: &'a str,
        }

        let include_plain = !(self.ansi_only && self.ansi.is_some());
        let plain = if include_plain {
            self.ansi.map(crate::terminal_text::strip_terminal_escapes)
        } else {
            None
        };
        let document = Document {
            agent_type: self.agent_type,
            columns: self.columns,
            id: self.id,
            label: self.label,
            rows: self.rows,
            session_title: self.title,
            status: self.status,
            tag: self.tag,
            tag_color: self.tag_color,
            tail: include_plain.then_some(plain.as_deref()),
            tail_ansi: self.ansi,
            tail_format: self.ansi.map(|_| "ansi-sgr"),
            updated_at: self.updated_at,
        };
        output.clear();
        // These fields cannot fail serialization and Vec's writer cannot fail.
        serde_json::to_writer(&mut *output, &document).expect("terminal frame JSON");
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(frame: &Frame) -> String {
        let mut document = serde_json::json!({
            "id": frame.id, "agentType": frame.agent_type, "status": frame.status,
            "sessionTitle": frame.title, "label": frame.label, "tag": frame.tag,
            "tagColor": frame.tag_color, "tailAnsi": frame.ansi,
            "tailFormat": frame.ansi.map(|_| "ansi-sgr"),
            "columns": frame.columns, "rows": frame.rows, "updatedAt": frame.updated_at,
        });
        if !(frame.ansi_only && frame.ansi.is_some()) {
            document["tail"] =
                serde_json::json!(frame.ansi.map(crate::terminal_text::strip_terminal_escapes));
        }
        document.to_string()
    }

    #[test]
    fn borrowed_frames_match_the_wire_format_and_reuse_storage() {
        let controls: String = (0u8..=31).map(char::from).collect();
        let large = "\x1b[31mГотово ✓ 日本語\x1b[0m \"quoted\"\\\n".repeat(1024);
        let mut output = Vec::new();
        for ansi in [
            Some(large.as_str()),
            None,
            Some(""),
            Some("hello"),
            Some("\x1bé"),
            Some(controls.as_str()),
            Some("\x1b]unterminated"),
        ] {
            for ansi_only in [false, true] {
                for title in [None, Some(""), Some("User's \"prompt\"\n☃\\")] {
                    for grid in [None, Some((80, 24)), Some((u16::MAX, u16::MAX))] {
                        let frame = Frame {
                            id: "sd_term_test",
                            agent_type: "shell",
                            status: "IDLE",
                            label: "● IDLE",
                            title,
                            tag: 5,
                            tag_color: Some("#ffee00"),
                            ansi,
                            columns: grid.map(|g| g.0),
                            rows: grid.map(|g| g.1),
                            ansi_only,
                            updated_at: "2026-09-27T00:00:00.000Z",
                        };
                        assert_eq!(frame.write_json(&mut output), reference(&frame).as_bytes());
                        let allocation = output.as_ptr();
                        let capacity = output.capacity();
                        let gone = Frame {
                            ansi: None,
                            status: "EXITED",
                            title: None,
                            tag: 0,
                            tag_color: None,
                            columns: None,
                            rows: None,
                            ..frame
                        };
                        assert_eq!(gone.write_json(&mut output), reference(&gone).as_bytes());
                        assert_eq!(output.as_ptr(), allocation);
                        assert_eq!(output.capacity(), capacity);
                    }
                }
            }
        }
    }
}
