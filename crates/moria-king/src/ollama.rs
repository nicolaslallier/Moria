//! Client Ollama : `POST {url}/api/chat`, sans flux ni réflexion, sortie JSON imposée.
use crate::Llm;
use crate::prompt::ChatMessage;
use moria_core::king::QuestId;
use std::time::Duration;

pub struct Ollama {
    pub url: String,
    pub model: String,
    pub timeout: Duration,
}

impl Llm for Ollama {
    async fn chat(&self, msgs: &[ChatMessage]) -> anyhow::Result<String> {
        let keys: Vec<serde_json::Value> = QuestId::ALL
            .iter()
            .map(|q| q.key().into())
            .chain([serde_json::Value::Null])
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "stream": false,
            "think": false,
            "format": {
                "type": "object",
                "properties": {
                    "say": { "type": "string" },
                    "offer": { "enum": keys },
                },
                "required": ["say", "offer"],
            },
            "options": { "temperature": 0.7 },
        });
        let url = format!("{}/api/chat", self.url.trim_end_matches('/'));
        let timeout = self.timeout;
        // ureq est bloquant : hors des threads du runtime.
        let v: serde_json::Value = tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .build()
                .into();
            anyhow::Ok(agent.post(&url).send_json(&body)?.body_mut().read_json()?)
        })
        .await??;
        v["message"]["content"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("réponse Ollama sans message.content"))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Lit une requête HTTP complète (en-têtes + corps selon Content-Length).
    async fn read_request(s: &mut TcpStream) -> (String, String) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).await.unwrap();
            assert!(n > 0, "requête tronquée");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let head = &text[..end];
                let len = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    let body = String::from_utf8(buf[end + 4..end + 4 + len].to_vec()).unwrap();
                    return (head.to_string(), body);
                }
            }
        }
    }

    /// Serveur d'un seul échange : renvoie la requête reçue, répond `reply` en JSON.
    pub(crate) async fn fake_ollama(
        reply: &'static str,
    ) -> (String, tokio::task::JoinHandle<(String, String)>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let req = read_request(&mut s).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            req
        });
        (url, h)
    }

    #[tokio::test]
    async fn ollama_client_posts_expected_body() {
        let (url, server) = fake_ollama(
            r#"{"message":{"role":"assistant","content":"{\"say\":\"ok\"}"},"done":true}"#,
        )
        .await;
        let o = Ollama {
            url,
            model: "gemma-test".into(),
            timeout: Duration::from_secs(5),
        };
        let msgs = vec![ChatMessage {
            role: "user",
            content: "bonjour".into(),
        }];
        assert_eq!(o.chat(&msgs).await.unwrap(), r#"{"say":"ok"}"#);
        let (head, body) = server.await.unwrap();
        assert!(head.starts_with("POST /api/chat "), "{head}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "gemma-test");
        assert_eq!(v["stream"], false);
        assert_eq!(v["think"], false);
        assert_eq!(v["format"]["type"], "object");
        assert_eq!(v["format"]["required"], serde_json::json!(["say", "offer"]));
        assert_eq!(
            v["messages"],
            serde_json::json!([{"role":"user","content":"bonjour"}])
        );
        assert!(v["options"]["temperature"].is_number());
    }

    #[tokio::test]
    async fn http_error_is_an_error() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            read_request(&mut s).await;
            s.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let o = Ollama {
            url,
            model: "m".into(),
            timeout: Duration::from_secs(5),
        };
        assert!(o.chat(&[]).await.is_err());
    }
}
