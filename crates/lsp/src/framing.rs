//! LSP 传输层：Content-Length 帧化的 JSON-RPC（base protocol）。
//!
//! 每条消息形如：
//! ```text
//! Content-Length: <N>\r\n
//! \r\n
//! <N 字节的 UTF-8 JSON>
//! ```
//! 本模块只管「把一个 JSON 值编成一帧字节」与「从一个 BufRead 里读出下一帧的 JSON」，
//! 不涉及 JSON-RPC 语义（id/method/result）——那是 client 的事，便于单测纯帧化逻辑。

use crate::LspError;
use std::io::BufRead;

/// 把一个 JSON 值编码为一条 LSP 帧（Content-Length 头 + CRLF 空行 + body）。
pub fn encode_frame(value: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    frame
}

/// 从一个带缓冲的读取器里读出下一条 LSP 帧，返回其 JSON body。
///
/// 解析头部直到空行，取出 `Content-Length`，再精确读取这么多字节作为 body 并解析为 JSON。
/// 流结束（EOF）或缺少 Content-Length 时返回清晰错误，绝不无限阻塞读取。
pub fn read_frame<R: BufRead>(reader: &mut R) -> Result<serde_json::Value, LspError> {
    let mut content_length: Option<usize> = None;

    // 1) 逐行读头部，直到遇到空行（CRLF 或 LF）。
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| LspError::Protocol(format!("读取响应头失败: {e}")))?;
        if n == 0 {
            return Err(LspError::Protocol(
                "语言服务器在读取响应前关闭了连接（EOF）".to_string(),
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // 头部结束
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| LspError::Protocol(format!("无效的 Content-Length: {value}")))
                    .map(Some)?;
            }
            // 其它头（如 Content-Type）忽略。
        }
    }

    let len = content_length
        .ok_or_else(|| LspError::Protocol("响应头缺少 Content-Length".to_string()))?;

    // 2) 精确读取 len 字节作为 body。
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| LspError::Protocol(format!("读取响应体失败（期望 {len} 字节）: {e}")))?;

    serde_json::from_slice(&body)
        .map_err(|e| LspError::Protocol(format!("响应体不是合法 JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn encode_then_parse_roundtrip() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "rootUri": "file:///ws", "nested": { "a": [1, 2, 3] } }
        });
        let frame = encode_frame(&msg);

        // 帧应以 Content-Length 头开头，并含 CRLF 空行分隔。
        let header_end = frame
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("帧必须含 CRLF 空行");
        let header = String::from_utf8(frame[..header_end].to_vec()).unwrap();
        assert!(header.starts_with("Content-Length: "));
        let declared: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // 声明长度应等于 body 实际字节数。
        assert_eq!(declared, frame.len() - header_end - 4);

        // 回环解析应还原原始 JSON。
        let mut reader = BufReader::new(&frame[..]);
        let parsed = read_frame(&mut reader).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn parse_handles_lowercase_header_and_extra_headers() {
        let body = serde_json::json!({ "ok": true });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        // 混入额外头 + 大小写不敏感的 content-length。
        let mut frame = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n",
            body_bytes.len()
        )
        .into_bytes();
        frame.extend_from_slice(&body_bytes);
        let mut reader = BufReader::new(&frame[..]);
        let parsed = read_frame(&mut reader).unwrap();
        assert_eq!(parsed, body);
    }

    #[test]
    fn parse_reads_two_consecutive_frames() {
        let a = serde_json::json!({ "id": 1 });
        let b = serde_json::json!({ "id": 2 });
        let mut buf = encode_frame(&a);
        buf.extend(encode_frame(&b));
        let mut reader = BufReader::new(&buf[..]);
        assert_eq!(read_frame(&mut reader).unwrap(), a);
        assert_eq!(read_frame(&mut reader).unwrap(), b);
    }

    #[test]
    fn parse_eof_is_clear_error() {
        let empty: &[u8] = b"";
        let mut reader = BufReader::new(empty);
        let err = read_frame(&mut reader).unwrap_err();
        assert!(matches!(err, LspError::Protocol(_)));
    }

    #[test]
    fn parse_missing_content_length_errors() {
        let frame = b"X-Foo: bar\r\n\r\n".to_vec();
        let mut reader = BufReader::new(&frame[..]);
        assert!(read_frame(&mut reader).is_err());
    }
}
