//! Provider authentication flows (OpenRouter OAuth PKCE & OpenAI API key helper).

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::CONTENT_TYPE;
use hyper::Request;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use ring::digest::{digest, SHA256};
use std::io::{self, Write};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Open a URL in the user's default browser.
pub fn open_browser(url: &str) {
    let _ = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "linux") {
        std::process::Command::new("xdg-open").arg(url).status()
    } else {
        std::process::Command::new("open").arg(url).status()
    };
}

/// Base64url encode bytes without padding.
fn base64url_encode(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() * 4).div_ceil(3));
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i] as usize;
        let b1 = if i + 1 < input.len() {
            input[i + 1] as usize
        } else {
            0
        };
        let b2 = if i + 2 < input.len() {
            input[i + 2] as usize
        } else {
            0
        };

        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[(triple >> 18) & 0x3F] as char);
        out.push(CHARS[(triple >> 12) & 0x3F] as char);
        if i + 1 < input.len() {
            out.push(CHARS[(triple >> 6) & 0x3F] as char);
        }
        if i + 2 < input.len() {
            out.push(CHARS[triple & 0x3F] as char);
        }
        i += 3;
    }
    out
}

/// Generate PKCE verifier and challenge.
pub fn generate_pkce() -> (String, String) {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();

    let mut random_bytes = Vec::with_capacity(64);
    for i in 0..64 {
        let byte = ((seed.wrapping_mul(6364136223846793005)
            ^ (pid as u128).wrapping_add(i as u128 * 0x9E3779B97F4A7C15))
            >> (i % 32)) as u8;
        random_bytes.push(byte);
    }

    let verifier = base64url_encode(&random_bytes)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(64)
        .collect::<String>();

    let hashed = digest(&SHA256, verifier.as_bytes());
    let challenge = base64url_encode(hashed.as_ref());
    (verifier, challenge)
}

/// Run OpenRouter PKCE OAuth flow using a local loopback server or manual code entry.
pub async fn openrouter_oauth_flow() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_string());
        }
    }

    let (verifier, challenge) = generate_pkce();

    println!("\n\x1b[1mOpenRouter OAuth Authentication\x1b[0m");
    println!("Choose authorization method:");
    println!("  \x1b[1m1)\x1b[0m Automatic browser redirect to localhost (desktop default)");
    println!("  \x1b[1m2)\x1b[0m Copy-paste authorization code from browser (headless / remote SSH)");
    print!("Selection [1/2, default 1]: ");
    io::stdout().flush()?;

    let mut choice = String::new();
    let _ = io::stdin().read_line(&mut choice);
    let use_manual = choice.trim() == "2";

    let auth_code = if use_manual {
        let auth_url = format!(
            "https://openrouter.ai/auth?code_challenge={challenge}&code_challenge_method=S256&key_label=Kobold"
        );
        println!("\nOpening browser at: \x1b[36m{auth_url}\x1b[0m");
        println!("1. Authorize Kobold in the OpenRouter window.");
        println!("2. Copy the authorization code displayed on screen.\n");
        open_browser(&auth_url);

        print!("Paste your OpenRouter authorization code (or existing 'sk-or-' key): ");
        io::stdout().flush()?;

        let mut code_input = String::new();
        io::stdin().read_line(&mut code_input)?;
        let trimmed = code_input.trim().to_string();
        if trimmed.is_empty() {
            return Err("no authorization code or key provided".into());
        }
        if trimmed.starts_with("sk-or-") {
            println!("\x1b[32m✓\x1b[0m Using provided OpenRouter API key.");
            return Ok(trimmed);
        }
        trimmed
    } else {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();

        let encoded_callback = format!("http%3A%2F%2Flocalhost%3A{port}%2Fcallback");
        let auth_url = format!(
            "https://openrouter.ai/auth?callback_url={encoded_callback}&code_challenge={challenge}&code_challenge_method=S256&key_label=Kobold"
        );

        println!("\x1b[36minfo:\x1b[0m Starting OpenRouter OAuth authentication...");
        println!("Opening browser at: {auth_url}");
        println!("Waiting for authorization from OpenRouter...\n");
        open_browser(&auth_url);

        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let (mut socket, _) = listener.accept().await?;
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await?;
                let req = String::from_utf8_lossy(&buf[..n]);

                if let Some(line) = req.lines().next() {
                    if line.contains("GET ") {
                        if let Some(path) = line.split_whitespace().nth(1) {
                            if let Some(query_idx) = path.find('?') {
                                let query = &path[query_idx + 1..];
                                for pair in query.split('&') {
                                    if let Some((k, v)) = pair.split_once('=') {
                                        if k == "code" {
                                            let code = v.to_string();
                                            let html = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Kobold Authenticated</title><style>body{font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",Roboto,sans-serif;background:#0f1419;color:#e6edf3;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;}.card{background:#161b22;border:1px solid #30363d;border-radius:12px;padding:40px 32px;text-align:center;max-width:440px;box-shadow:0 8px 24px rgba(0,0,0,0.5);}.icon{font-size:44px;color:#3fb950;margin-bottom:16px;}h2{margin:0 0 12px;font-size:22px;color:#fff;}p{margin:0;font-size:14px;color:#8b949e;line-height:1.6;}</style></head><body><div class=\"card\"><div class=\"icon\">&#10003;</div><h2>Kobold Authenticated</h2><p>OpenRouter authorization received.<br>You may close this tab and return to the terminal.</p></div></body></html>";
                                            let resp = format!(
                                                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                                html.len(),
                                                html
                                            );
                                            let _ = socket.write_all(resp.as_bytes()).await;
                                            let _ = socket.flush().await;
                                            return Ok::<String, io::Error>(code);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let not_found = "HTTP/1.1 404 NOT FOUND\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(not_found.as_bytes()).await;
            }
        })
        .await??
    };

    println!("\x1b[32m✓\x1b[0m Authorization code received. Exchanging for API key...");

    // Exchange code for API key via OpenRouter API
    let https: HttpsConnector<HttpConnector> = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build(https);

    let payload = serde_json::json!({
        "code": auth_code,
        "code_verifier": verifier,
        "code_challenge_method": "S256"
    });
    let json_bytes = serde_json::to_vec(&payload)?;

    let req = Request::post("https://openrouter.ai/api/v1/auth/keys")
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(json_bytes)))?;

    let res = client.request(req).await?;
    let status = res.status();
    let body_bytes = res.into_body().collect().await?.to_bytes();
    let body_str = String::from_utf8_lossy(&body_bytes);

    if !status.is_success() {
        return Err(format!("OpenRouter key exchange failed ({status}): {body_str}").into());
    }

    let parsed: serde_json::Value = serde_json::from_str(&body_str)?;
    if let Some(key) = parsed.get("key").and_then(|k| k.as_str()) {
        Ok(key.to_string())
    } else {
        Err(format!("invalid response from OpenRouter: {body_str}").into())
    }
}

/// Prompt for OpenAI API key with a direct link to the API key management page.
pub fn prompt_openai_key() -> io::Result<String> {
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            println!("\x1b[32m✓\x1b[0m Detected OPENAI_API_KEY from environment.");
            return Ok(key.trim().to_string());
        }
    }

    println!("\n\x1b[1mOpenAI API Configuration\x1b[0m");
    println!("OpenAI requires an API key to access its Realtime and Responses APIs.");
    println!("Opening OpenAI Platform API keys dashboard in your browser:");
    println!("  \x1b[36mhttps://platform.openai.com/api-keys\x1b[0m\n");
    open_browser("https://platform.openai.com/api-keys");

    print!("Paste your OpenAI API Key (or press Enter to set later): ");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_generation_produces_valid_pair() {
        let (verifier, challenge) = generate_pkce();
        assert!(verifier.len() >= 43 && verifier.len() <= 128);
        assert!(!challenge.is_empty());
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
        assert!(!challenge.contains('='));
    }
}
