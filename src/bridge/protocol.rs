use serde::{Deserialize, Serialize};

/// Messages sent from the client (container) to the server (host).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Run {
        id: String,
        cmd: Vec<String>,
        cwd: Option<String>,
    },
    Signal {
        id: String,
        signal: String,
    },
    Stdin {
        id: String,
        data: String,
    },
}

/// Messages sent from the server (host) to the client (container).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Started { id: String, pid: u32 },
    Stdout { id: String, data: String },
    Stderr { id: String, data: String },
    Exit { id: String, code: i32 },
    Error { id: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_run_message() {
        let json = r#"{"type":"run","id":"1","cmd":["xcodebuild","-project","Foo.xcodeproj"],"cwd":"/tmp"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Run { id, cmd, cwd } => {
                assert_eq!(id, "1");
                assert_eq!(cmd, vec!["xcodebuild", "-project", "Foo.xcodeproj"]);
                assert_eq!(cwd, Some("/tmp".to_string()));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_deserialize_run_no_cwd() {
        let json = r#"{"type":"run","id":"2","cmd":["adb","devices"]}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Run { id, cwd, .. } => {
                assert_eq!(id, "2");
                assert!(cwd.is_none());
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_deserialize_signal_message() {
        let json = r#"{"type":"signal","id":"1","signal":"SIGINT"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Signal { .. }));
    }

    #[test]
    fn test_deserialize_stdin_message() {
        let json = r#"{"type":"stdin","id":"1","data":"yes\n"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Stdin { .. }));
    }

    #[test]
    fn test_serialize_started_message() {
        let msg = ServerMessage::Started {
            id: "1".into(),
            pid: 12345,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"started""#));
        assert!(json.contains(r#""pid":12345"#));
    }

    #[test]
    fn test_serialize_stdout_message() {
        let msg = ServerMessage::Stdout {
            id: "1".into(),
            data: "hello\n".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"stdout""#));
        assert!(json.contains(r#""data":"hello\n""#));
    }

    #[test]
    fn test_serialize_exit_message() {
        let msg = ServerMessage::Exit {
            id: "1".into(),
            code: 0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"exit""#));
        assert!(json.contains(r#""code":0"#));
    }

    #[test]
    fn test_serialize_error_message() {
        let msg = ServerMessage::Error {
            id: "1".into(),
            message: "not allowed".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
    }
}
