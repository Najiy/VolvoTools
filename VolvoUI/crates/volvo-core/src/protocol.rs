//! JSON-lines protocol spoken by `VolvoFlasher --json` (see docs/tech_specs/ui_json_protocol.md).
//!
//! Every structured line on stdout is one JSON object with a `type` tag. Anything else the
//! flasher prints (library logging, legacy text) is passed through as a raw log line.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub library: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Span {
    pub start: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkPlan {
    pub start: u64,
    pub length: u64,
    /// Bytes of this chunk that differ from the input file because the flasher corrected checksums.
    #[serde(default)]
    pub modified: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumInfo {
    /// The flasher knows a checksum scheme for this image size and ECU.
    pub supported: bool,
    /// The input file already satisfies that scheme.
    pub valid: bool,
    /// How many checksum records were really evaluated. The ME7-style check passes without
    /// looking at anything when its table is not in the expected format, so `valid` alone
    /// proves nothing; zero here means exactly that.
    #[serde(default)]
    pub checked_blocks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootloaderPlan {
    pub call: u64,
    pub chunks: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub platform: String,
    pub ecu: String,
    pub cm_info: String,
    pub file_size: u64,
    pub checksum: ChecksumInfo,
    pub bootloader: BootloaderPlan,
    pub erase: Vec<Span>,
    pub chunks: Vec<ChunkPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    Hello {
        protocol: u32,
        #[serde(default)]
        app: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        command: String,
    },
    Devices {
        devices: Vec<Device>,
    },
    State {
        state: String,
    },
    Progress {
        current: u64,
        max: u64,
    },
    Plan(Plan),
    Result {
        ok: bool,
        #[serde(default)]
        message: String,
    },
    /// A well-formed object with a `type` this build does not know. Ignored for forward compatibility.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Message(Message),
    Raw(String),
}

pub fn parse_line(line: &str) -> Line {
    let trimmed = line.trim();
    if trimmed.starts_with('{')
        && trimmed.ends_with('}')
        && let Ok(message) = serde_json::from_str::<Message>(trimmed)
    {
        return Line::Message(message);
    }
    Line::Raw(line.to_string())
}

pub fn to_line(message: &Message) -> String {
    serde_json::to_string(message).expect("protocol messages always serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_message_type() {
        assert_eq!(
            parse_line(
                r#"{"type":"hello","protocol":1,"app":"VolvoFlasher","version":"1.0","command":"read"}"#
            ),
            Line::Message(Message::Hello {
                protocol: 1,
                app: "VolvoFlasher".into(),
                version: "1.0".into(),
                command: "read".into()
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"state","state":"EraseFlash"}"#),
            Line::Message(Message::State {
                state: "EraseFlash".into()
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"progress","current":10,"max":200}"#),
            Line::Message(Message::Progress {
                current: 10,
                max: 200
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"result","ok":false,"message":"boom"}"#),
            Line::Message(Message::Result {
                ok: false,
                message: "boom".into()
            })
        );
        let devices = parse_line(
            r#"{"type":"devices","devices":[{"name":"DiCE-1","library":"C:\\x\\y.dll"}]}"#,
        );
        assert_eq!(
            devices,
            Line::Message(Message::Devices {
                devices: vec![Device {
                    name: "DiCE-1".into(),
                    library: "C:\\x\\y.dll".into()
                }]
            })
        );
    }

    #[test]
    fn parses_a_plan() {
        let line = concat!(
            r#"{"type":"plan","platform":"P1","ecu":"7A","cmInfo":"me9_p1","fileSize":2097152,"#,
            r#""checksum":{"supported":true,"valid":false,"checkedBlocks":60},"#,
            r#""bootloader":{"call":8356304,"chunks":[{"start":8356304,"length":11496}]},"#,
            r#""erase":[{"start":131072,"length":0}],"#,
            r#""chunks":[{"start":131072,"length":458752,"modified":0},{"start":655360,"length":1376256,"modified":6}]}"#
        );
        let Line::Message(Message::Plan(plan)) = parse_line(line) else {
            panic!("expected a plan");
        };
        assert_eq!(plan.cm_info, "me9_p1");
        assert_eq!(plan.chunks[1].modified, 6);
        assert!(plan.checksum.supported && !plan.checksum.valid);
        assert_eq!(plan.checksum.checked_blocks, 60);
    }

    #[test]
    fn a_plan_without_checked_blocks_reports_none() {
        let line = concat!(
            r#"{"type":"plan","platform":"P1","ecu":"7A","cmInfo":"me9_p1","fileSize":1,"#,
            r#""checksum":{"supported":true,"valid":true},"#,
            r#""bootloader":{"call":1,"chunks":[]},"erase":[],"chunks":[]}"#
        );
        let Line::Message(Message::Plan(plan)) = parse_line(line) else {
            panic!("expected a plan");
        };
        assert_eq!(plan.checksum.checked_blocks, 0);
    }

    #[test]
    fn unknown_type_is_ignored_not_raw() {
        assert_eq!(
            parse_line(r#"{"type":"future","x":1}"#),
            Line::Message(Message::Unknown)
        );
    }

    #[test]
    fn anything_else_is_raw() {
        for text in ["Starting", "", "{not json}", r#"{"no":"type"}"#, "[1,2]"] {
            assert_eq!(parse_line(text), Line::Raw(text.to_string()), "{text:?}");
        }
    }

    #[test]
    fn round_trips_through_to_line() {
        let message = Message::Progress { current: 3, max: 9 };
        assert_eq!(parse_line(&to_line(&message)), Line::Message(message));
    }
}
