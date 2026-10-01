//! JSON-RPC 2.0 framing for plugin API 1: one JSON object per line, at most
//! 1 MiB (`skills/super-desktop-plugin/schemas/host-api.openrpc.json`, `x-transport`).
use serde_json::{json, Value};

pub const MAX_MESSAGE: usize = 1024 * 1024;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const PERMISSION_DENIED: i64 = -32001;
pub const NOT_FOUND: i64 = -32002;
pub const LIMIT_EXCEEDED: i64 = -32003;
pub const UNAVAILABLE: i64 = -32004;
pub const TIMEOUT: i64 = -32005;
pub const CANCELLED: i64 = -32006;

/// An error answer. Host errors always carry `data: {reason, hint, docs}`.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, reason: impl Into<String>, hint: impl Into<String>, docs: &str) -> Self {
        let message = match code {
            PARSE_ERROR => "parse_error",
            INVALID_REQUEST => "invalid_request",
            METHOD_NOT_FOUND => "method_not_found",
            INVALID_PARAMS => "invalid_params",
            PERMISSION_DENIED => "permission_denied",
            NOT_FOUND => "not_found",
            LIMIT_EXCEEDED => "limit_exceeded",
            UNAVAILABLE => "unavailable",
            TIMEOUT => "timeout",
            CANCELLED => "cancelled",
            _ => "error",
        };
        RpcError {
            code,
            message: message.into(),
            data: Some(json!({"reason": reason.into(), "hint": hint.into(), "docs": docs})),
        }
    }

    pub fn invalid_params(reason: impl Into<String>, docs: &str) -> Self {
        Self::new(INVALID_PARAMS, reason, "Compare the params with schemas/host-api.openrpc.json.", docs)
    }

    pub fn reason(&self) -> String {
        self.data
            .as_ref()
            .and_then(|d| d["reason"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.message.clone())
    }

    pub fn to_value(&self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }

    fn from_value(value: &Value) -> Self {
        RpcError {
            code: value["code"].as_i64().unwrap_or(-32000),
            message: value["message"].as_str().unwrap_or("error").to_string(),
            data: value.get("data").cloned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
    Response { id: Value, result: Result<Value, RpcError> },
}

/// Parse one line. `Err` carries the error to answer with (and the id, when
/// one could be read) — or `None` when the line is not even worth answering.
pub fn parse(line: &str) -> Result<Message, (Option<Value>, RpcError)> {
    const DOCS: &str = "references/host-api.md#transport";
    if line.len() > MAX_MESSAGE {
        return Err((None, RpcError::new(LIMIT_EXCEEDED, "message larger than 1 MiB", "Split the data (ui.patch) or shorten it.", DOCS)));
    }
    let value: Value = serde_json::from_str(line).map_err(|error| {
        (None, RpcError::new(PARSE_ERROR, format!("not JSON: {error}"), "Write exactly one JSON object per line on stdout, nothing else; log to stderr.", DOCS))
    })?;
    let invalid = |id: Option<Value>, why: &str| {
        (id, RpcError::new(INVALID_REQUEST, why, "Send {\"jsonrpc\": \"2.0\", \"method\": …, \"params\": {…}} with an id for requests.", DOCS))
    };
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid(value.get("id").cloned(), "missing \"jsonrpc\": \"2.0\""));
    }
    let id = value.get("id").filter(|id| id.is_string() || id.is_number()).cloned();
    if let Some(method) = value.get("method") {
        let Some(method) = method.as_str() else {
            return Err(invalid(id, "method must be a string"));
        };
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Err((id, RpcError::invalid_params("params must be an object (named parameters)", DOCS)));
        }
        return Ok(match id {
            Some(id) => Message::Request { id, method: method.to_string(), params },
            None => Message::Notification { method: method.to_string(), params },
        });
    }
    let Some(id) = id else {
        return Err(invalid(None, "neither a request nor a response"));
    };
    if let Some(error) = value.get("error") {
        return Ok(Message::Response { id, result: Err(RpcError::from_value(error)) });
    }
    Ok(Message::Response { id, result: Ok(value.get("result").cloned().unwrap_or(Value::Null)) })
}

pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

pub fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

pub fn response(id: &Value, result: Result<Value, RpcError>) -> String {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error.to_value()}).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_rpc_parses_the_three_kinds() {
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","id":3,"method":"log","params":{"level":"info","message":"x"}}"#).unwrap(),
            Message::Request { id: json!(3), method: "log".into(), params: json!({"level":"info","message":"x"}) }
        );
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","method":"log"}"#).unwrap(),
            Message::Notification { method: "log".into(), params: json!({}) }
        );
        assert_eq!(parse(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#).unwrap(), Message::Response { id: json!("a"), result: Ok(json!({})) });
        let Message::Response { result: Err(error), .. } = parse(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#).unwrap() else {
            panic!()
        };
        assert_eq!(error.code, METHOD_NOT_FOUND);
    }

    #[test]
    fn plugin_rpc_rejects_with_hints() {
        for (line, code) in [
            ("hello", PARSE_ERROR),
            (r#"{"id":1,"method":"log"}"#, INVALID_REQUEST),
            (r#"{"jsonrpc":"2.0","id":1,"method":"log","params":[1]}"#, INVALID_PARAMS),
            (r#"{"jsonrpc":"2.0"}"#, INVALID_REQUEST),
        ] {
            let (_, error) = parse(line).unwrap_err();
            assert_eq!(error.code, code, "{line}");
            let data = error.data.unwrap();
            assert!(data["hint"].as_str().is_some_and(|h| !h.is_empty()) && data["docs"].as_str().unwrap().starts_with("references/"));
        }
        let big = format!(r#"{{"jsonrpc":"2.0","method":"log","params":{{"message":"{}"}}}}"#, "x".repeat(MAX_MESSAGE));
        assert_eq!(parse(&big).unwrap_err().1.code, LIMIT_EXCEEDED);
    }
}
