//! 测试用的假 HTTP 服务器：tokio 裸 `TcpListener` 手撸一个够用的 HTTP/1.1。
//!
//! 为什么不引 wiremock：就为了断言「请求路径 + 参数形状」再加一棵依赖树不值当，
//! Go 版 `obs/obs_test.go` 也是这个套路（假服务器 + 验请求体）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// 不含 query
    pub path: String,
    /// 不含 `?`
    pub query: String,
    /// 键统一小写
    pub headers: HashMap<String, String>,
    /// body 的 UTF-8 近似（`from_utf8_lossy`）。看表单、看文本够用，
    /// **二进制（multipart 里那张真图片）会变成问号** —— 要比原始字节就用 `body_raw`。
    pub body: String,
    /// body 的原始字节。multipart 上传那条链只有它能钉死「文件内容原样传上去了」。
    pub body_raw: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == name).then_some(v)
        })
    }
}

pub struct FakeServer {
    pub base: String,
    hits: Arc<Mutex<Vec<Request>>>,
}

impl FakeServer {
    pub fn hits(&self) -> Vec<Request> {
        self.hits.lock().unwrap().clone()
    }
}

/// 起一个假服务器，每个请求的响应由 `responder` 决定（状态码 + body）。
pub async fn start<F>(responder: F) -> FakeServer
where
    F: Fn(&Request) -> (u16, String) + Send + Sync + 'static,
{
    start_with_headers(move |r| {
        let (status, body) = responder(r);
        (status, body, Vec::new())
    })
    .await
}

/// 跟 `start` 一样，但能额外给几个响应头（形如 `"set-cookie: a=b; Path=/"`）。
///
/// 单开一个入口只因为登录那条链的凭据就藏在 `Set-Cookie` 里 —— `(status, body)`
/// 表达不出来，而给所有测试换签名会把几十处调用一起搅动。
pub async fn start_with_headers<F>(responder: F) -> FakeServer
where
    F: Fn(&Request) -> (u16, String, Vec<String>) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑不上本地端口");
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let responder = Arc::new(responder);

    let sink = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let sink = sink.clone();
            let responder = responder.clone();
            tokio::spawn(async move {
                let Some(req) = read_request(&mut sock).await else {
                    return;
                };
                let (status, body, extra) = responder(&req);
                sink.lock().unwrap().push(req);
                let extra: String = extra.iter().map(|h| format!("{h}\r\n")).collect();
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n{extra}\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });

    FakeServer {
        base: format!("http://{addr}"),
        hits,
    }
}

async fn read_request(sock: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let want: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < want {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(want);

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    Some(Request {
        method,
        path,
        query,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
        body_raw: body,
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
