//! # wpm — the Weft package registry
//!
//! Like npm, with the one difference that changes everything: **packages
//! cannot lie**. Every Weft definition is named by the hash of its canonical
//! bytes, so this server holds no authority — it is a well-lit shelf. Every
//! upload is verified before it is stored (hashes match, exports resolve,
//! the whole def set passes the Weft verifier), and every consumer verifies
//! again locally. Mirrors are equals; the registry can vanish and nothing
//! published breaks.
//!
//! Routes (HTTP/1.1, TLS at the proxy):
//!   GET  /                                       → index JSON (all packages)
//!   GET  /healthz                                → 200 ok
//!   GET  /favicon.ico                            → the icon (browsers ask for it)
//!   GET  /packages/<name>.weftpack.json          → the package
//!   GET  /.well-known/weft/<name>.weftpack.json  → same (the Thread convention)
//!   POST /publish                                → body = package JSON;
//!        verified before storage. If WPM_TOKEN is set, requires
//!        `authorization: Bearer <token>`; unset = open shelf (dev).
//!
//! Env: PORT (default 3000 — matches the Coolify template), WPM_DATA
//! (default ./data), WPM_TOKEN (optional publish gate).
//! Boot seeds `seed/*.weftpack.json` into an empty data dir.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use weft::pack::Package;

struct App {
    data: PathBuf,
    token: Option<String>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3000);
    let data = PathBuf::from(std::env::var("WPM_DATA").unwrap_or_else(|_| "data".into()));
    std::fs::create_dir_all(&data).expect("data dir");

    // Seed an empty shelf with the bundled packages (each verified first —
    // the registry never vouches for bytes it hasn't checked).
    if std::fs::read_dir(&data).map(|mut d| d.next().is_none()).unwrap_or(true) {
        if let Ok(entries) = std::fs::read_dir("seed") {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.ends_with(".weftpack.json") {
                    continue;
                }
                match std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<Package>(&t).ok().map(|p| (t, p)))
                {
                    Some((text, pkg)) if pkg.verify().is_ok() => {
                        let _ = std::fs::write(data.join(&name), text);
                        println!("seeded {name} ('{}')", pkg.name);
                    }
                    _ => eprintln!("seed {name} REFUSED (invalid or unverifiable)"),
                }
            }
        }
    }

    let app = Arc::new(App {
        data,
        token: std::env::var("WPM_TOKEN").ok().filter(|t| !t.is_empty()),
    });
    let listener = TcpListener::bind(("0.0.0.0", port)).await.expect("bind");
    println!("wpm listening on :{port} — the shelf is open");
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let app = app.clone();
        tokio::spawn(async move {
            let _ = handle(stream, app).await;
        });
    }
}

async fn handle(mut stream: TcpStream, app: Arc<App>) -> std::io::Result<()> {
    // Read the head + as much body as declared (publishes are small).
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];
    let head_end;
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            head_end = pos + 4;
            break;
        }
        if buf.len() > 64 * 1024 {
            return respond(&mut stream, 431, "text/plain", b"head too large").await;
        }
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (request.next().unwrap_or(""), request.next().unwrap_or("/"));
    let path = path.split('?').next().unwrap_or(path).to_string();
    let header = |name: &str| -> Option<String> {
        head.lines()
            .find(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")))
            .map(|l| l[name.len() + 1..].trim().to_string())
    };

    match (method, path.as_str()) {
        ("GET", "/healthz") => respond(&mut stream, 200, "text/plain", b"ok").await,
        ("GET", "/robots.txt") => respond(&mut stream, 200, "text/plain; charset=utf-8", ROBOTS.as_bytes()).await,
        ("GET", "/favicon.ico") => respond(&mut stream, 200, "image/x-icon", FAVICON).await,
        ("GET", "/sitemap.xml") => {
            let body = sitemap_xml(&listing(&app.data));
            respond(&mut stream, 200, "application/xml", body.as_bytes()).await
        }
        ("GET", "/") => {
            let list = listing(&app.data);
            // A browser or a crawler says so in Accept; curl and every tool that
            // reads this index says `*/*` and keeps the JSON it has always had.
            if wants_html(header("accept").as_deref()) {
                let body = index_html(&list);
                return respond(&mut stream, 200, "text/html; charset=utf-8", body.as_bytes()).await;
            }
            let packages: Vec<_> = list
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "file": p.file,
                        "url": format!("/packages/{}", p.file),
                        "exports": p.exports,
                        "defs": p.defs,
                    })
                })
                .collect();
            let body = serde_json::json!({
                "registry": "wpm",
                "motto": "packages that cannot lie — verify locally, always",
                "spec": "https://github.com/Pixygon/Infinite/blob/main/docs/spec/weft-pack-v0.1.md",
                "packages": packages,
            });
            respond(&mut stream, 200, "application/json", body.to_string().as_bytes()).await
        }
        // One readable page per package, so the shelf has something Google can
        // index and a person can link to. The bytes still live at /packages/.
        ("GET", p) if p.starts_with("/p/") => {
            let slug = p.trim_start_matches("/p/").trim_end_matches('/');
            let file = format!("{slug}.weftpack.json");
            if !clean_name(&file) {
                return respond(&mut stream, 404, "text/html; charset=utf-8", not_found_html().as_bytes()).await;
            }
            match std::fs::read_to_string(app.data.join(&file))
                .ok()
                .and_then(|t| serde_json::from_str::<Package>(&t).ok())
            {
                Some(pkg) => {
                    let mut exports: Vec<String> = pkg.exports.keys().cloned().collect();
                    exports.sort();
                    let listed = Listed {
                        name: pkg.name.clone(),
                        file: file.clone(),
                        exports,
                        defs: pkg.defs.len(),
                    };
                    let body = package_html(&listed, &pkg);
                    respond(&mut stream, 200, "text/html; charset=utf-8", body.as_bytes()).await
                }
                None => respond(&mut stream, 404, "text/html; charset=utf-8", not_found_html().as_bytes()).await,
            }
        }
        ("GET", p) => {
            let name = p
                .strip_prefix("/packages/")
                .or_else(|| p.strip_prefix("/.well-known/weft/"));
            match name.filter(|n| clean_name(n)) {
                Some(n) => match std::fs::read(app.data.join(n)) {
                    Ok(bytes) => respond(&mut stream, 200, "application/json", &bytes).await,
                    Err(_) => respond(&mut stream, 404, "text/plain", b"no such package").await,
                },
                None => respond(&mut stream, 404, "text/plain", b"not found").await,
            }
        }
        ("POST", "/publish") => {
            if let Some(tok) = &app.token {
                let ok = header("authorization")
                    .is_some_and(|a| a.strip_prefix("Bearer ") == Some(tok.as_str()));
                if !ok {
                    return respond(&mut stream, 401, "text/plain", b"bad token").await;
                }
            }
            let want: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            if want == 0 || want > 4 * 1024 * 1024 {
                return respond(&mut stream, 413, "text/plain", b"bad content-length (max 4MB)").await;
            }
            let mut body = buf[head_end..].to_vec();
            while body.len() < want {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&tmp[..n]);
            }
            let Ok(text) = String::from_utf8(body) else {
                return respond(&mut stream, 400, "text/plain", b"not utf-8").await;
            };
            let Ok(pkg) = serde_json::from_str::<Package>(&text) else {
                return respond(&mut stream, 400, "text/plain", b"not a weft package").await;
            };
            // The gate that matters: full verification before storage.
            if let Err(e) = pkg.verify() {
                let msg = format!("REFUSED: {e}");
                return respond(&mut stream, 422, "text/plain", msg.as_bytes()).await;
            }
            let file = format!("{}.weftpack.json", slug(&pkg.name));
            if !clean_name(&file) {
                return respond(&mut stream, 400, "text/plain", b"bad package name").await;
            }
            std::fs::write(app.data.join(&file), &text)?;
            println!("published '{}' → {file} ({} defs)", pkg.name, pkg.defs.len());
            let body = serde_json::json!({ "ok": true, "url": format!("/packages/{file}") });
            respond(&mut stream, 200, "application/json", body.to_string().as_bytes()).await
        }
        _ => respond(&mut stream, 405, "text/plain", b"method not allowed").await,
    }
}

/// Registry filenames: kebab alnum + the exact suffix — nothing traverses.
fn clean_name(n: &str) -> bool {
    n.strip_suffix(".weftpack.json").is_some_and(|s| {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

fn slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    ctype: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        _ => "",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\naccess-control-allow-origin: *\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

// ─── the human half of the shelf ─────────────────────────────────────────
//
// The registry answered `GET /` with JSON and nothing else: no HTML anywhere,
// no robots.txt, no sitemap. To a person or a crawler wpm.pixygon.io was a wall
// of braces, so Google had nothing to index and a visitor had nothing to read.
//
// The API is untouched. HTML is served ONLY when the client explicitly asks for
// `text/html` in Accept — which browsers and Googlebot do and `curl` does not —
// so every tool that reads the index keeps getting exactly the bytes it got
// before.

const SITE: &str = "https://wpm.pixygon.io";
const SPONSOR: &str = "https://github.com/sponsors/Pixygon";

/// One package as the index knows it.
struct Listed {
    name: String,
    file: String,
    exports: Vec<String>,
    defs: usize,
}

fn listing(data: &std::path::Path) -> Vec<Listed> {
    let mut list = Vec::new();
    if let Ok(entries) = std::fs::read_dir(data) {
        for e in entries.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            if !file.ends_with(".weftpack.json") {
                continue;
            }
            if let Ok(pkg) = serde_json::from_str::<Package>(
                &std::fs::read_to_string(e.path()).unwrap_or_default(),
            ) {
                let mut exports: Vec<String> = pkg.exports.keys().cloned().collect();
                exports.sort();
                list.push(Listed { name: pkg.name, file, exports, defs: pkg.defs.len() });
            }
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// Minimal escaping — package names and export names only, but they come from
/// uploads, and an upload is not allowed to write markup into our page.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn wants_html(accept: Option<&str>) -> bool {
    accept.is_some_and(|a| a.to_ascii_lowercase().contains("text/html"))
}

fn page(title: &str, desc: &str, canonical: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{t}</title>
<meta name="description" content="{d}">
<link rel="canonical" href="{c}">
<meta property="og:type" content="website">
<meta property="og:title" content="{t}">
<meta property="og:description" content="{d}">
<meta property="og:url" content="{c}">
<style>
:root{{color-scheme:dark;--void:#07080c;--void2:#0d0f16;--line:#1e2230;--star:#e8ecf5;--dim:#8b93a7;--cyan:#5ce1e6}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--void);color:var(--star);font:16px/1.6 ui-sans-serif,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}}
.wrap{{max-width:860px;margin:0 auto;padding:0 20px}}
header{{border-bottom:1px solid var(--line);padding:14px 0}}
header .wrap{{display:flex;align-items:center;gap:14px}}
.brand{{font-weight:700;letter-spacing:.1em;color:var(--cyan)}}
header nav{{margin-left:auto;display:flex;gap:16px;font-size:14px}}
a{{color:var(--cyan);text-decoration:none}}
a:hover{{text-decoration:underline}}
h1{{font-size:2rem;line-height:1.2;margin:36px 0 10px;text-wrap:balance}}
h2{{font-size:1.15rem;margin:34px 0 10px}}
p.lede{{color:var(--dim);font-size:1.05rem;margin:0 0 8px}}
code,pre{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}}
pre{{background:var(--void2);border:1px solid var(--line);border-radius:10px;padding:14px 16px;overflow-x:auto;font-size:.9rem}}
ul.pkgs{{list-style:none;padding:0;margin:0;display:grid;gap:10px}}
ul.pkgs li{{border:1px solid var(--line);border-radius:10px;padding:14px 16px;background:var(--void2)}}
ul.pkgs .n{{font-weight:700}}
ul.pkgs .meta{{color:var(--dim);font-size:.86rem;margin-top:4px}}
table{{width:100%;border-collapse:collapse;font-size:.92rem}}
td,th{{text-align:left;padding:7px 10px;border-bottom:1px solid var(--line);vertical-align:top}}
th{{color:var(--dim);font-weight:600}}
footer{{border-top:1px solid var(--line);margin-top:48px;padding:22px 0 40px;color:var(--dim);font-size:.9rem}}
</style>
</head><body>
<header><div class="wrap">
  <span class="brand">WPM</span>
  <nav>
    <a href="/">Packages</a>
    <a href="https://thread.pixygon.io">Thread</a>
    <a href="{s}">&hearts; Sponsor</a>
  </nav>
</div></header>
<main class="wrap">
{b}
</main>
<footer><div class="wrap">
  wpm is the package registry for <b>Weft</b>, built by <a href="https://pixygon.io">Pixygon</a>.
  Free and open &mdash; <a href="{s}">sponsoring</a> is what keeps it on the shelf.
</div></footer>
</body></html>"#,
        t = esc(title), d = esc(desc), c = esc(canonical), s = SPONSOR, b = body,
    )
}

fn index_html(list: &[Listed]) -> String {
    let count = list.len();
    let items = if list.is_empty() {
        "<p class=\"lede\">The shelf is empty right now.</p>".to_string()
    } else {
        let rows: String = list
            .iter()
            .map(|p| {
                format!(
                    "<li><div class=\"n\"><a href=\"/p/{slug}\">{name}</a></div>\
                     <div class=\"meta\">{defs} definition{ds} · exports {ex}</div></li>",
                    slug = esc(p.file.trim_end_matches(".weftpack.json")),
                    name = esc(&p.name),
                    defs = p.defs,
                    ds = if p.defs == 1 { "" } else { "s" },
                    ex = esc(&p.exports.join(", ")),
                )
            })
            .collect();
        format!("<ul class=\"pkgs\">{rows}</ul>")
    };
    let body = format!(
        r#"<h1>Packages that cannot lie</h1>
<p class="lede">wpm is the package registry for <b>Weft</b>. Every definition is named by the hash
of its own canonical bytes, so this server holds no authority over what it serves &mdash; it is a
well-lit shelf. Every upload is verified before it is stored, and every consumer verifies again
locally. Mirrors are equals; if this registry vanished, nothing already published would break.</p>

<h2>Install a package</h2>
<pre>weft add &lt;name&gt;          # resolves through this registry
weft verify              # checks every hash locally, always</pre>

<h2>{count} package{s} on the shelf</h2>
{items}

<h2>Read it as JSON</h2>
<p>Every page here has a machine-readable twin. The index is this same URL asked for as JSON,
and each package is its raw <code>.weftpack.json</code>:</p>
<pre>curl {site}/
curl {site}/packages/&lt;name&gt;.weftpack.json</pre>

<h2>Publish</h2>
<p><code>POST /publish</code> with the package JSON as the body. It is verified in full before
it is stored: hashes must match, exports must resolve, and the whole definition set must pass
the Weft verifier. A package that cannot prove itself is refused, not shelved.</p>"#,
        count = count,
        s = if count == 1 { "" } else { "s" },
        items = items,
        site = SITE,
    );
    page(
        "wpm — the Weft package registry",
        "wpm is the package registry for the Weft language: content-addressed packages verified before they are stored and again by every consumer. Free and open, from Pixygon.",
        &format!("{SITE}/"),
        &body,
    )
}

fn package_html(p: &Listed, pkg: &Package) -> String {
    let slug = p.file.trim_end_matches(".weftpack.json");
    let exports: String = if p.exports.is_empty() {
        "<tr><td colspan=\"2\">No exports.</td></tr>".to_string()
    } else {
        p.exports
            .iter()
            .map(|e| {
                let hash = pkg.exports.get(e).map(|h| h.to_string()).unwrap_or_default();
                format!("<tr><td><code>{}</code></td><td><code>{}</code></td></tr>", esc(e), esc(&hash))
            })
            .collect()
    };
    let body = format!(
        r#"<h1>{name}</h1>
<p class="lede">A Weft package on the wpm shelf: {defs} definition{ds}, {nex} export{es}.
Content-addressed, so the name below IS the check &mdash; fetch it anywhere and verify it yourself.</p>

<h2>Add it</h2>
<pre>weft add {name}
weft verify</pre>

<h2>Exports</h2>
<table><tr><th>Export</th><th>Hash</th></tr>{exports}</table>

<h2>The bytes</h2>
<p><a href="/packages/{file}">{file}</a> &mdash; the raw package, the same bytes every consumer
verifies. <a href="/">Back to all packages</a>.</p>"#,
        name = esc(&p.name),
        defs = p.defs,
        ds = if p.defs == 1 { "" } else { "s" },
        nex = p.exports.len(),
        es = if p.exports.len() == 1 { "" } else { "s" },
        exports = exports,
        file = esc(&p.file),
    );
    page(
        &format!("{} — a Weft package on wpm", p.name),
        &format!(
            "{}: a content-addressed Weft package with {} definition{} on the wpm registry. Verified before storage, verifiable by you.",
            p.name, p.defs, if p.defs == 1 { "" } else { "s" },
        ),
        &format!("{SITE}/p/{slug}"),
        &body,
    )
}

fn not_found_html() -> String {
    page(
        "Not a package here — wpm",
        "No package by that name is on the wpm shelf.",
        &format!("{SITE}/"),
        "<h1>Nothing on this shelf</h1>\n<p class=\"lede\">No package answers to that name. \
<a href=\"/\">See everything wpm holds</a>.</p>",
    )
}

fn sitemap_xml(list: &[Listed]) -> String {
    let urls: String = list
        .iter()
        .map(|p| {
            format!(
                "  <url>\n    <loc>{SITE}/p/{}</loc>\n    <changefreq>weekly</changefreq>\n    <priority>0.7</priority>\n  </url>\n",
                esc(p.file.trim_end_matches(".weftpack.json")),
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n  <url>\n    <loc>{SITE}/</loc>\n    <changefreq>daily</changefreq>\n    <priority>1.0</priority>\n  </url>\n{urls}</urlset>\n"
    )
}

const ROBOTS: &str = "User-agent: *\nAllow: /\n\nSitemap: https://wpm.pixygon.io/sitemap.xml\n";

/// The shelf's mark, baked into the binary. The root is JSON and cannot
/// declare an icon, so browsers fall back to /favicon.ico and logged a 404.
const FAVICON: &[u8] = include_bytes!("../public/favicon.ico");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favicon_is_a_real_ico() {
        // ICO header: reserved 0, type 1 (icon), then at least one image.
        assert_eq!(&FAVICON[..4], &[0, 0, 1, 0]);
        assert!(u16::from_le_bytes([FAVICON[4], FAVICON[5]]) >= 1);
    }

    #[tokio::test]
    async fn favicon_route_answers_200_with_the_icon() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Arc::new(App { data: std::env::temp_dir(), token: None });
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            handle(s, app).await.unwrap();
        });
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"GET /favicon.ico HTTP/1.1\r\nhost: x\r\n\r\n").await.unwrap();
        let mut out = Vec::new();
        c.read_to_end(&mut out).await.unwrap();
        let head_end = find(&out, b"\r\n\r\n").unwrap() + 4;
        let head = String::from_utf8_lossy(&out[..head_end]);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("content-type: image/x-icon"), "{head}");
        assert_eq!(&out[head_end..], FAVICON);
    }
}
