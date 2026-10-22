//! Google sign-in for an installed app: the browser asks the user, Google
//! sends them back to a one-off listener on 127.0.0.1 with a code, and the
//! code (with the PKCE verifier only this process knows) becomes a refresh
//! token. <https://developers.google.com/identity/protocols/oauth2/native-app>

use crate::error::{Error, Result, summary};
use crate::http::{Body, Method, Request, Transport};
use crate::keychain::Credentials;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Upload, and read the channel's name and its videos (to find one again).
pub const SCOPES: &str =
    "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly";

/// A sign-in under way: the browser goes to `url`, and [`SignIn::finish`]
/// waits for it to come back.
pub struct SignIn {
    pub url: String,
    listener: TcpListener,
    redirect: String,
    verifier: String,
    state: String,
}

/// Opens the listener and writes the address the browser is sent to.
pub fn begin(client_id: &str) -> Result<SignIn> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err(Error::Config("the OAuth client id is empty".into()));
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| Error::SignIn(format!("cannot listen for the sign-in: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| Error::SignIn(err.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random_token(48)?;
    let state = random_token(16)?;
    let url = authorize_url(client_id, &redirect, &challenge(&verifier), &state);
    Ok(SignIn {
        url,
        listener,
        redirect,
        verifier,
        state,
    })
}

pub fn authorize_url(client_id: &str, redirect: &str, challenge: &str, state: &str) -> String {
    let query = form(&[
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        // A refresh token, and a fresh one each time: the old may have lapsed.
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]);
    format!("{AUTH_URL}?{query}")
}

impl SignIn {
    /// Waits for the browser to come back, then trades its code for a
    /// refresh token. Gives up after `timeout`.
    pub fn finish<T: Transport>(
        self,
        transport: &T,
        client_id: &str,
        client_secret: &str,
        timeout: Duration,
    ) -> Result<Credentials> {
        let code = self.wait(timeout)?;
        let answer = token_call(
            transport,
            &[
                ("code", code.as_str()),
                ("client_id", client_id.trim()),
                ("client_secret", client_secret.trim()),
                ("redirect_uri", &self.redirect),
                ("grant_type", "authorization_code"),
                ("code_verifier", &self.verifier),
            ],
        )?;
        let refresh_token = answer["refresh_token"]
            .as_str()
            .ok_or_else(|| Error::Response("Google sent no refresh token".into()))?;
        Ok(Credentials {
            client_id: client_id.trim().to_string(),
            client_secret: client_secret.trim().to_string(),
            refresh_token: refresh_token.to_string(),
        })
    }

    fn wait(&self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        self.listener
            .set_nonblocking(true)
            .map_err(|err| Error::SignIn(err.to_string()))?;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(answer) = self.answer(stream) {
                        return answer;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(Error::SignIn(
                            "the sign-in was not finished in the browser in time".into(),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(Error::SignIn(err.to_string())),
            }
        }
    }

    /// Reads one request; None if it was not the sign-in coming back.
    fn answer(&self, stream: TcpStream) -> Option<Result<String>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        let Some(answer) = parse_callback(&line, &self.state) else {
            reply(stream, "404 Not Found", "");
            return None;
        };
        let words = match &answer {
            Ok(_) => "Signed in to YouTube. You can close this tab and go back to Stet.",
            Err(_) => "The YouTube sign-in did not finish. Go back to Stet and try again.",
        };
        reply(stream, "200 OK", words);
        Some(answer)
    }
}

fn reply(mut stream: TcpStream, status: &str, words: &str) {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Stet</title>\
         <body style=\"font:16px -apple-system,sans-serif;margin:4em auto;max-width:32em\">{words}</body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
}

/// The code in a request line like `GET /?state=..&code=.. HTTP/1.1`.
/// None for anything else the browser asks for (a favicon).
pub fn parse_callback(request_line: &str, state: &str) -> Option<Result<String>> {
    let target = request_line.split_whitespace().nth(1)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return None;
    }
    let pairs: Vec<(String, String)> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect();
    let get = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if let Some(error) = get("error") {
        let said = match error {
            "access_denied" => "the sign-in was declined in the browser".to_string(),
            other => format!("Google refused the sign-in: {other}"),
        };
        return Some(Err(Error::SignIn(said)));
    }
    if get("state") != Some(state) {
        return Some(Err(Error::SignIn("the sign-in came back for another request".into())));
    }
    Some(
        get("code")
            .filter(|code| !code.is_empty())
            .map(str::to_string)
            .ok_or_else(|| Error::SignIn("the sign-in came back without a code".into())),
    )
}

/// An access token for the next hour, from the stored refresh token.
pub fn access_token<T: Transport>(transport: &T, credentials: &Credentials) -> Result<String> {
    let answer = token_call(
        transport,
        &[
            ("client_id", &credentials.client_id),
            ("client_secret", &credentials.client_secret),
            ("refresh_token", &credentials.refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;
    answer["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Response("Google sent no access token".into()))
}

fn token_call<T: Transport>(transport: &T, fields: &[(&str, &str)]) -> Result<serde_json::Value> {
    let request = Request {
        method: Method::Post,
        url: TOKEN_URL.into(),
        headers: vec![("Content-Type".into(), "application/x-www-form-urlencoded".into())],
        body: Body::Bytes(form(fields).into_bytes()),
    };
    let response = transport.send(&request)?;
    let value: Option<serde_json::Value> = serde_json::from_str(&response.body).ok();
    let error = value.as_ref().and_then(|v| v["error"].as_str());
    match response.status {
        200..=299 => value.ok_or_else(|| Error::Response("the token answer is not JSON".into())),
        // A refresh token that was revoked or lapsed, or a client that was deleted.
        400 | 401 if matches!(error, Some("invalid_grant" | "invalid_client" | "unauthorized_client")) => {
            Err(Error::SignedOut(summary(&response.body)))
        }
        status => Err(Error::Status {
            status,
            body: summary(&response.body),
        }),
    }
}

/// The S256 PKCE challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|err| Error::SignIn(format!("no randomness: {err}")))?;
    Ok(base64url(&buf))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// `application/x-www-form-urlencoded`, which is also a query string.
pub fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::Mock;

    fn credentials() -> Credentials {
        Credentials {
            client_id: "id.apps.googleusercontent.com".into(),
            client_secret: "shh".into(),
            refresh_token: "1//refresh".into(),
        }
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // Appendix B of RFC 7636.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_authorize_address_asks_for_offline_upload() {
        let url = authorize_url("id", "http://127.0.0.1:5000", "chal", "st");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?client_id=id&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
        assert!(url.contains("youtube.upload"));
        assert!(url.contains("code_challenge=chal&code_challenge_method=S256&state=st"));
        assert!(url.contains("access_type=offline"));
    }

    #[test]
    fn callbacks_are_read() {
        let ok = parse_callback("GET /?state=s1&code=4%2F0Ab HTTP/1.1\r\n", "s1");
        assert_eq!(ok.unwrap().unwrap(), "4/0Ab");
        assert!(parse_callback("GET /favicon.ico HTTP/1.1", "s1").is_none());
        let declined = parse_callback("GET /?error=access_denied&state=s1 HTTP/1.1", "s1");
        assert!(declined.unwrap().unwrap_err().to_string().contains("declined"));
        let forged = parse_callback("GET /?state=other&code=x HTTP/1.1", "s1");
        assert!(forged.unwrap().is_err());
        let empty = parse_callback("GET /?state=s1 HTTP/1.1", "s1");
        assert!(empty.unwrap().is_err());
    }

    #[test]
    fn the_browser_comes_back_to_the_listener() {
        let sign_in = begin("id").unwrap();
        let port = sign_in.redirect.rsplit(':').next().unwrap().to_string();
        let state = sign_in.state.clone();
        let browser = std::thread::spawn(move || {
            let get = |path: &str| {
                let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
                write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut page = String::new();
                let _ = std::io::Read::read_to_string(&mut stream, &mut page);
                page
            };
            assert!(get("/favicon.ico").starts_with("HTTP/1.1 404"));
            get(&format!("/?code=the-code&state={state}"))
        });
        let mock = Mock::default().reply(200, r#"{"access_token":"a","refresh_token":"1//r","expires_in":3599}"#);
        let credentials = sign_in
            .finish(&&mock, " id ", "secret", Duration::from_secs(10))
            .unwrap();
        assert!(browser.join().unwrap().contains("Signed in to YouTube"));
        assert_eq!(credentials.refresh_token, "1//r");
        assert_eq!(credentials.client_id, "id");
        let sent = mock.sent.borrow();
        assert_eq!(sent[0].url, TOKEN_URL);
        let body = sent[0].text().unwrap();
        assert!(body.contains("code=the-code"), "{body}");
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code_verifier="));
    }

    #[test]
    fn a_sign_in_nobody_finishes_times_out() {
        let sign_in = begin("id").unwrap();
        let err = sign_in
            .finish(&&Mock::default(), "id", "s", Duration::from_millis(150))
            .unwrap_err();
        assert!(err.to_string().contains("in time"), "{err}");
    }

    #[test]
    fn refreshing_gives_an_access_token() {
        let mock = Mock::default().reply(200, r#"{"access_token":"ya29.x","expires_in":3599}"#);
        assert_eq!(access_token(&&mock, &credentials()).unwrap(), "ya29.x");
        let body = mock.sent.borrow()[0].text().unwrap().to_string();
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=1%2F%2Frefresh"));
    }

    #[test]
    fn a_lapsed_refresh_token_means_signed_out() {
        let mock = Mock::default().reply(
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        );
        let err = access_token(&&mock, &credentials()).unwrap_err();
        assert!(matches!(err, Error::SignedOut(_)), "{err}");
        assert!(err.to_string().contains("expired or revoked"));
    }

    #[test]
    fn forms_are_encoded_and_decoded() {
        assert_eq!(form(&[("a b", "x/y+z"), ("k", "~ok")]), "a%20b=x%2Fy%2Bz&k=~ok");
        assert_eq!(decode("a+b%2Fc%zz%"), "a b/c%zz%");
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
    }
}
